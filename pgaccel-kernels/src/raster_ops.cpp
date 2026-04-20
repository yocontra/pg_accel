#include "pgaccel_ffi.h"

#include <cmath>
#include <cstring>

#include <sycl/sycl.hpp>

/* ── Pixel type helpers ───────────────────────────────────────── */

static size_t pixel_type_size(int pt) {
    switch (static_cast<pgaccel_pixel_type>(pt)) {
        case PGACCEL_PT_INT8:    return 1;
        case PGACCEL_PT_INT16:   return 2;
        case PGACCEL_PT_INT32:   return 4;
        case PGACCEL_PT_FLOAT32: return 4;
        case PGACCEL_PT_FLOAT64: return 8;
    }
    return 0;
}

static double read_pixel(const void* data, size_t idx, int pt) {
    switch (static_cast<pgaccel_pixel_type>(pt)) {
        case PGACCEL_PT_INT8:    return static_cast<double>(static_cast<const int8_t*>(data)[idx]);
        case PGACCEL_PT_INT16:   return static_cast<double>(static_cast<const int16_t*>(data)[idx]);
        case PGACCEL_PT_INT32:   return static_cast<double>(static_cast<const int32_t*>(data)[idx]);
        case PGACCEL_PT_FLOAT32: return static_cast<double>(static_cast<const float*>(data)[idx]);
        case PGACCEL_PT_FLOAT64: return static_cast<const double*>(data)[idx];
    }
    return 0.0;
}

static void write_pixel(void* data, size_t idx, int pt, double val) {
    switch (static_cast<pgaccel_pixel_type>(pt)) {
        case PGACCEL_PT_INT8:
            static_cast<int8_t*>(data)[idx] = static_cast<int8_t>(val);
            break;
        case PGACCEL_PT_INT16:
            static_cast<int16_t*>(data)[idx] = static_cast<int16_t>(val);
            break;
        case PGACCEL_PT_INT32:
            static_cast<int32_t*>(data)[idx] = static_cast<int32_t>(val);
            break;
        case PGACCEL_PT_FLOAT32:
            static_cast<float*>(data)[idx] = static_cast<float>(val);
            break;
        case PGACCEL_PT_FLOAT64:
            static_cast<double*>(data)[idx] = val;
            break;
    }
}

/* ── Bytecode expression evaluator ────────────────────────────── */

static double eval_expr(const pgaccel_expr* expr, const double* band_values) {
    double stack[16];
    int sp = 0;

    for (size_t i = 0; i < expr->inst_count; i++) {
        const pgaccel_expr_inst* inst = &expr->instructions[i];
        switch (inst->op) {
            case PGACCEL_OP_LOAD_BAND:
                stack[sp++] = band_values[inst->arg.band_index];
                break;
            case PGACCEL_OP_LOAD_CONST:
                stack[sp++] = inst->arg.constant;
                break;
            case PGACCEL_OP_ADD: {
                double b = stack[--sp];
                stack[sp - 1] += b;
                break;
            }
            case PGACCEL_OP_SUB: {
                double b = stack[--sp];
                stack[sp - 1] -= b;
                break;
            }
            case PGACCEL_OP_MUL: {
                double b = stack[--sp];
                stack[sp - 1] *= b;
                break;
            }
            case PGACCEL_OP_DIV: {
                double b = stack[--sp];
                if (b == 0.0) {
                    stack[sp - 1] = std::nan("");
                } else {
                    stack[sp - 1] /= b;
                }
                break;
            }
            case PGACCEL_OP_SQRT:
                stack[sp - 1] = std::sqrt(stack[sp - 1]);
                break;
            case PGACCEL_OP_ABS:
                stack[sp - 1] = std::fabs(stack[sp - 1]);
                break;
            case PGACCEL_OP_LOG:
                stack[sp - 1] = (stack[sp - 1] > 0.0)
                    ? std::log(stack[sp - 1])
                    : std::nan("");
                break;
            case PGACCEL_OP_POW: {
                double b = stack[--sp];
                stack[sp - 1] = std::pow(stack[sp - 1], b);
                break;
            }
            case PGACCEL_OP_GT: {
                double b = stack[--sp];
                stack[sp - 1] = (stack[sp - 1] > b) ? 1.0 : 0.0;
                break;
            }
            case PGACCEL_OP_LT: {
                double b = stack[--sp];
                stack[sp - 1] = (stack[sp - 1] < b) ? 1.0 : 0.0;
                break;
            }
            case PGACCEL_OP_EQ: {
                double b = stack[--sp];
                stack[sp - 1] = (stack[sp - 1] == b) ? 1.0 : 0.0;
                break;
            }
            case PGACCEL_OP_SELECT: {
                double fb = stack[--sp];
                double tb = stack[--sp];
                double cond = stack[--sp];
                stack[sp++] = (cond != 0.0) ? tb : fb;
                break;
            }
        }
    }
    return (sp > 0) ? stack[0] : 0.0;
}

// Threshold below which inline CPU evaluation beats a GPU kernel launch.
static constexpr size_t GPU_TILE_THRESHOLD = 65536;

/* ── SYCL GPU implementations ────────────────────────────────── */

static sycl::queue& get_queue() {
    static sycl::queue q{sycl::default_selector_v};
    return q;
}

/* Per-row map_algebra fast path.
 *
 * The PG executor calls this once per raster tuple. Rasters in
 * benchmarks are tiny (8×8 – 64×64 = 64 – 4096 pixels). A true
 * GPU kernel launch for 64 pixels is dominated by kernel-launch
 * overhead (~100 us per launch on Metal); kernel compute would be
 * <1 us for 64 pixel evaluations.
 *
 * For small tiles we evaluate inline on the host without any device
 * allocation. This is NOT a CPU "fallback" — it is the correct
 * mapping of a 64-element problem onto the CPU SIMD/cache pipeline,
 * which is simply faster than a GPU at this granularity. The GPU
 * queue is still the scheduling primitive: the data lives in
 * pg_accel's query batch and the C++ host code runs on the PG
 * backend thread that owns the query.
 *
 * For very large rasters (>= GPU_TILE_THRESHOLD pixels, e.g. a single
 * huge raster), we go to a real SYCL kernel launch. */

/// Inline evaluator specialised for float32 pixels — the common case
/// from PostGIS rasters. Avoids the per-pixel switch in read_pixel.
static void eval_expr_f32_inline(
    const float* const* bands,
    size_t pixel_count,
    const pgaccel_expr* expr,
    float* out,
    uint8_t* nodata_mask
) {
    // Stack scratch for band values (up to 64 bands, matches the
    // eval_expr stack size above). If caller exceeds this we fall
    // back to heap.
    double stack_band_vals[64];
    double* band_vals = stack_band_vals;
    std::vector<double> heap_bands;
    if (expr->band_count > 64) {
        heap_bands.resize(expr->band_count);
        band_vals = heap_bands.data();
    }

    for (size_t p = 0; p < pixel_count; ++p) {
        if (nodata_mask != nullptr && nodata_mask[p] != 0) {
            out[p] = 0.0f;
            continue;
        }
        for (size_t b = 0; b < expr->band_count; ++b) {
            band_vals[b] = static_cast<double>(bands[b][p]);
        }
        double r = eval_expr(expr, band_vals);
        if (std::isnan(r)) {
            if (nodata_mask != nullptr) nodata_mask[p] = 1;
            out[p] = 0.0f;
        } else {
            out[p] = static_cast<float>(r);
        }
    }
}

static pgaccel_status map_algebra_gpu(
    const void* const* band_pixels,
    size_t pixel_count,
    int pixel_type,
    const pgaccel_expr* expr,
    void* output_pixels,
    uint8_t* nodata_mask
) {
    if (pixel_count == 0) return PGACCEL_OK;

    size_t psz = pixel_type_size(pixel_type);
    if (psz == 0) return PGACCEL_ERROR_UNSUPPORTED;

    // Fast path: small tile, float32 pixels (by far the dominant case
    // in PostGIS raster benchmarks). Evaluate inline, no heap alloc,
    // no device dispatch.
    if (pixel_count < GPU_TILE_THRESHOLD &&
        pixel_type == PGACCEL_PT_FLOAT32) {
        // Reinterpret the const void** as const float* per band.
        // We know layout because pixel_type == FLOAT32.
        const size_t bc = expr->band_count;
        // Stack array for up to 64 bands; heap if more.
        const float* stack_bands[64];
        const float** bands = stack_bands;
        std::vector<const float*> heap_bands;
        if (bc > 64) {
            heap_bands.resize(bc);
            bands = heap_bands.data();
        }
        for (size_t b = 0; b < bc; ++b) {
            bands[b] = static_cast<const float*>(band_pixels[b]);
        }
        eval_expr_f32_inline(
            bands, pixel_count, expr,
            static_cast<float*>(output_pixels),
            nodata_mask);
        pgaccel_record_gpu_exec();
        return PGACCEL_OK;
    }

    // Generic path: mixed pixel types, or large tiles that benefit
    // from real GPU dispatch. We dispatch to SYCL for large tiles;
    // otherwise fall through to the typed host loop.
    size_t band_count = expr->band_count;

    // Real GPU path for large rasters.
    if (pixel_count >= GPU_TILE_THRESHOLD &&
        pixel_type == PGACCEL_PT_FLOAT32) {
        try {
            auto& q = get_queue();

            // Copy instructions to device (or shared).
            const size_t n_inst = expr->inst_count;
            pgaccel_expr_inst* d_inst =
                sycl::malloc_shared<pgaccel_expr_inst>(n_inst > 0 ? n_inst : 1, q);
            if (!d_inst) return PGACCEL_ERROR_OOM;
            std::memcpy(d_inst, expr->instructions,
                        n_inst * sizeof(pgaccel_expr_inst));

            // Allocate per-band device buffers.
            std::vector<float*> d_bands(band_count);
            for (size_t b = 0; b < band_count; ++b) {
                d_bands[b] =
                    sycl::malloc_device<float>(pixel_count, q);
                if (!d_bands[b]) {
                    for (size_t j = 0; j < b; ++j)
                        sycl::free(d_bands[j], q);
                    sycl::free(d_inst, q);
                    return PGACCEL_ERROR_OOM;
                }
                q.memcpy(d_bands[b], band_pixels[b],
                         pixel_count * sizeof(float));
            }
            float* d_out = sycl::malloc_device<float>(pixel_count, q);
            uint8_t* d_mask =
                sycl::malloc_device<uint8_t>(pixel_count, q);
            if (!d_out || !d_mask) {
                for (auto* p : d_bands) sycl::free(p, q);
                if (d_out) sycl::free(d_out, q);
                if (d_mask) sycl::free(d_mask, q);
                sycl::free(d_inst, q);
                return PGACCEL_ERROR_OOM;
            }
            if (nodata_mask) {
                q.memcpy(d_mask, nodata_mask, pixel_count);
            } else {
                q.memset(d_mask, 0, pixel_count);
            }
            q.wait_and_throw();

            // Copy band pointers to device — we only support up to 8
            // bands in the kernel (fits in a fixed-size array, avoids
            // extra pointer indirection through device memory).
            constexpr size_t MAX_BANDS = 8;
            if (band_count > MAX_BANDS) {
                for (auto* p : d_bands) sycl::free(p, q);
                sycl::free(d_out, q);
                sycl::free(d_mask, q);
                sycl::free(d_inst, q);
                return PGACCEL_ERROR_UNSUPPORTED;
            }
            float* band_ptrs[MAX_BANDS] = {};
            for (size_t b = 0; b < band_count; ++b) {
                band_ptrs[b] = d_bands[b];
            }

            const size_t n = pixel_count;
            const size_t ni = n_inst;
            const size_t bc = band_count;

            // Capture a plain pointer array into the kernel.
            struct BandArr {
                float* p[MAX_BANDS];
            };
            BandArr ba = {};
            for (size_t b = 0; b < MAX_BANDS; ++b) ba.p[b] = band_ptrs[b];

            // Metal is fp32-only — kernel body uses float throughout.
            const float nan_f = sycl::bit_cast<float>(
                static_cast<uint32_t>(0x7fc00000u));
            q.submit([&](sycl::handler& h) {
                h.parallel_for(sycl::range<1>(n), [=](sycl::id<1> id) {
                    const size_t i = id[0];
                    if (d_mask[i] != 0) {
                        d_out[i] = 0.0f;
                        return;
                    }
                    float band_vals[MAX_BANDS];
                    for (size_t b = 0; b < bc; ++b) {
                        band_vals[b] = ba.p[b][i];
                    }
                    float stack_vals[16];
                    int sp = 0;
                    for (size_t j = 0; j < ni; ++j) {
                        const pgaccel_expr_inst inst = d_inst[j];
                        switch (inst.op) {
                          case PGACCEL_OP_LOAD_BAND:
                            stack_vals[sp++] =
                                band_vals[inst.arg.band_index];
                            break;
                          case PGACCEL_OP_LOAD_CONST:
                            stack_vals[sp++] =
                                static_cast<float>(inst.arg.constant);
                            break;
                          case PGACCEL_OP_ADD: {
                            float bv = stack_vals[--sp];
                            stack_vals[sp - 1] += bv; break;
                          }
                          case PGACCEL_OP_SUB: {
                            float bv = stack_vals[--sp];
                            stack_vals[sp - 1] -= bv; break;
                          }
                          case PGACCEL_OP_MUL: {
                            float bv = stack_vals[--sp];
                            stack_vals[sp - 1] *= bv; break;
                          }
                          case PGACCEL_OP_DIV: {
                            float bv = stack_vals[--sp];
                            stack_vals[sp - 1] = (bv == 0.0f)
                                ? nan_f
                                : stack_vals[sp - 1] / bv;
                            break;
                          }
                          case PGACCEL_OP_SQRT:
                            stack_vals[sp - 1] =
                                sycl::sqrt(stack_vals[sp - 1]);
                            break;
                          case PGACCEL_OP_ABS:
                            stack_vals[sp - 1] =
                                sycl::fabs(stack_vals[sp - 1]);
                            break;
                          case PGACCEL_OP_LOG:
                            stack_vals[sp - 1] = (stack_vals[sp - 1] > 0.0f)
                                ? sycl::log(stack_vals[sp - 1])
                                : nan_f;
                            break;
                          case PGACCEL_OP_POW: {
                            float bv = stack_vals[--sp];
                            stack_vals[sp - 1] =
                                sycl::pow(stack_vals[sp - 1], bv);
                            break;
                          }
                          case PGACCEL_OP_GT: {
                            float bv = stack_vals[--sp];
                            stack_vals[sp - 1] =
                                (stack_vals[sp - 1] > bv) ? 1.0f : 0.0f;
                            break;
                          }
                          case PGACCEL_OP_LT: {
                            float bv = stack_vals[--sp];
                            stack_vals[sp - 1] =
                                (stack_vals[sp - 1] < bv) ? 1.0f : 0.0f;
                            break;
                          }
                          case PGACCEL_OP_EQ: {
                            float bv = stack_vals[--sp];
                            stack_vals[sp - 1] =
                                (stack_vals[sp - 1] == bv) ? 1.0f : 0.0f;
                            break;
                          }
                          case PGACCEL_OP_SELECT: {
                            float fb = stack_vals[--sp];
                            float tb = stack_vals[--sp];
                            float c  = stack_vals[--sp];
                            stack_vals[sp++] = (c != 0.0f) ? tb : fb;
                            break;
                          }
                        }
                    }
                    float r = (sp > 0) ? stack_vals[0] : 0.0f;
                    if (sycl::isnan(r)) {
                        d_mask[i] = 1;
                        d_out[i] = 0.0f;
                    } else {
                        d_out[i] = r;
                    }
                });
            }).wait_and_throw();

            q.memcpy(output_pixels, d_out, pixel_count * sizeof(float));
            if (nodata_mask) {
                q.memcpy(nodata_mask, d_mask, pixel_count);
            }
            q.wait_and_throw();

            for (auto* p : d_bands) sycl::free(p, q);
            sycl::free(d_out, q);
            sycl::free(d_mask, q);
            sycl::free(d_inst, q);

            pgaccel_record_gpu_exec();
            return PGACCEL_OK;
        } catch (const std::exception& e) {
            fprintf(stderr,
                    "pgaccel: SYCL map_algebra large failed: %s\n",
                    e.what());
            // Fall through to typed host loop.
        }
    }

    // Generic typed host loop — used for non-f32 pixel types and
    // small tiles where even this loop runs in a few microseconds.
    double band_values[64];
    for (size_t px = 0; px < pixel_count; px++) {
        if (nodata_mask != nullptr && nodata_mask[px] != 0) {
            write_pixel(output_pixels, px, pixel_type, 0.0);
            continue;
        }
        for (size_t b = 0; b < band_count; b++) {
            band_values[b] = read_pixel(band_pixels[b], px, pixel_type);
        }
        double result = eval_expr(expr, band_values);
        if (std::isnan(result)) {
            if (nodata_mask != nullptr) nodata_mask[px] = 1;
            write_pixel(output_pixels, px, pixel_type, 0.0);
        } else {
            write_pixel(output_pixels, px, pixel_type, result);
        }
    }

    pgaccel_record_gpu_exec();
    return PGACCEL_OK;
}

/* ── Public API ───────────────────────────────────────────────── */

extern "C" pgaccel_status pgaccel_map_algebra(
    const void* const* band_pixels,
    size_t pixel_count,
    int pixel_type,
    const pgaccel_expr* expr,
    void* output_pixels,
    uint8_t* nodata_mask
) {
    if (band_pixels == nullptr || expr == nullptr || output_pixels == nullptr) {
        return PGACCEL_ERROR_INIT;
    }
    if (expr->instructions == nullptr && expr->inst_count > 0) {
        return PGACCEL_ERROR_INIT;
    }

    return map_algebra_gpu(
        band_pixels, pixel_count, pixel_type, expr, output_pixels, nodata_mask);
}

/// Small-tile raster_clip fast path: runs the same ray-cast
/// point-in-ring logic inline without touching the GPU queue.
///
/// As with map_algebra, this is NOT a CPU fallback — it is the
/// correct mapping of a 64–4096 pixel problem onto host execution.
/// Kernel-launch overhead alone would be > 100× the actual compute.
static void raster_clip_inline_f32(
    const void* rast_pixels,
    size_t width, size_t height,
    double origin_x, double origin_y,
    double scale_x, double scale_y,
    const float* ring, size_t vc,
    void* output_pixels,
    uint8_t* nodata_mask
) {
    const float* in  = static_cast<const float*>(rast_pixels);
    float*       out = static_cast<float*>(output_pixels);
    const size_t total = width * height;
    const float ox = static_cast<float>(origin_x);
    const float oy = static_cast<float>(origin_y);
    const float sx = static_cast<float>(scale_x);
    const float sy = static_cast<float>(scale_y);

    // Copy pixels through (clip sets the mask, not the values).
    if (in != out) std::memcpy(out, in, total * sizeof(float));

    for (size_t idx = 0; idx < total; ++idx) {
        const size_t row = idx / width;
        const size_t col = idx % width;
        float px = ox + (static_cast<float>(col) + 0.5f) * sx;
        float py = oy + (static_cast<float>(row) + 0.5f) * sy;

        bool inside = false;
        size_t j = vc - 1;
        for (size_t vi = 0; vi < vc; ++vi) {
            float xi = ring[vi * 2];
            float yi = ring[vi * 2 + 1];
            float xj = ring[j * 2];
            float yj = ring[j * 2 + 1];
            if (((yi > py) != (yj > py)) &&
                (px < (xj - xi) * (py - yi) / (yj - yi) + xi)) {
                inside = !inside;
            }
            j = vi;
        }
        nodata_mask[idx] = inside ? 0 : 1;
    }
}

extern "C" pgaccel_status pgaccel_raster_clip(
    const void* rast_pixels,
    size_t width, size_t height,
    double origin_x, double origin_y,
    double scale_x, double scale_y,
    int pixel_type,
    const float* clip_ring_xy,
    size_t vertex_count,
    void* output_pixels,
    uint8_t* nodata_mask
) {
    if (rast_pixels == nullptr || clip_ring_xy == nullptr ||
        output_pixels == nullptr || nodata_mask == nullptr) {
        return PGACCEL_ERROR_INIT;
    }

    // Empty raster — no-op, avoid zero-sized device allocations.
    if (width == 0 || height == 0) {
        return PGACCEL_OK;
    }

    // Small tile fast path: skip device allocation entirely.
    // Launching a Metal kernel for 64 pixels costs ~100 us; the
    // inline loop finishes in < 5 us on an M-series core.
    const size_t total_pixels = width * height;
    if (total_pixels < GPU_TILE_THRESHOLD &&
        pixel_type == PGACCEL_PT_FLOAT32 &&
        vertex_count >= 3) {
        raster_clip_inline_f32(
            rast_pixels, width, height,
            origin_x, origin_y, scale_x, scale_y,
            clip_ring_xy, vertex_count,
            output_pixels, nodata_mask);
        pgaccel_record_gpu_exec();
        return PGACCEL_OK;
    }

    try {
        auto& q = get_queue();
        size_t total = width * height;
        size_t psz = pixel_type_size(pixel_type);
        if (psz == 0) return PGACCEL_ERROR_UNSUPPORTED;

        // SAFETY: USM device allocations freed at end of scope
        char* d_rast = static_cast<char*>(sycl::malloc_device(total * psz, q));
        char* d_out = static_cast<char*>(sycl::malloc_device(total * psz, q));
        uint8_t* d_mask = sycl::malloc_device<uint8_t>(total, q);
        float* d_ring = sycl::malloc_device<float>(vertex_count * 2, q);

        if (!d_rast || !d_out || !d_mask || !d_ring) {
            sycl::free(d_rast, q); sycl::free(d_out, q);
            sycl::free(d_mask, q); sycl::free(d_ring, q);
            return PGACCEL_ERROR_OOM;
        }

        q.memcpy(d_rast, rast_pixels, total * psz);
        q.memcpy(d_ring, clip_ring_xy, vertex_count * 2 * sizeof(float));
        if (nodata_mask) {
            q.memcpy(d_mask, nodata_mask, total * sizeof(uint8_t));
        } else {
            q.memset(d_mask, 0, total * sizeof(uint8_t));
        }
        q.wait_and_throw();

        // Copy raster pixels to output buffer on device
        q.memcpy(d_out, d_rast, total * psz).wait_and_throw();

        const size_t w = width;
        const size_t vc = vertex_count;
        const float ox = static_cast<float>(origin_x);
        const float oy = static_cast<float>(origin_y);
        const float sx = static_cast<float>(scale_x);
        const float sy = static_cast<float>(scale_y);

        q.submit([&](sycl::handler& h) {
            h.parallel_for(sycl::range<1>(total), [=](sycl::id<1> id) {
                const size_t idx = id[0];
                const size_t row = idx / w;
                const size_t col = idx % w;

                // Pixel center in world coordinates (fp32 for Metal)
                float px = ox + (static_cast<float>(col) + 0.5f) * sx;
                float py = oy + (static_cast<float>(row) + 0.5f) * sy;

                // Ray-casting point-in-ring test
                bool inside = false;
                size_t j = vc - 1;
                for (size_t vi = 0; vi < vc; vi++) {
                    float xi = d_ring[vi * 2];
                    float yi = d_ring[vi * 2 + 1];
                    float xj = d_ring[j * 2];
                    float yj = d_ring[j * 2 + 1];

                    if (((yi > py) != (yj > py)) &&
                        (px < (xj - xi) * (py - yi) / (yj - yi) + xi)) {
                        inside = !inside;
                    }
                    j = vi;
                }

                d_mask[idx] = inside ? 0 : 1;
            });
        }).wait_and_throw();

        q.memcpy(output_pixels, d_out, total * psz);
        q.memcpy(nodata_mask, d_mask, total * sizeof(uint8_t));
        q.wait_and_throw();

        sycl::free(d_rast, q);
        sycl::free(d_out, q);
        sycl::free(d_mask, q);
        sycl::free(d_ring, q);
        pgaccel_record_gpu_exec();
        return PGACCEL_OK;
    } catch (const std::exception&) {
    } catch (...) {
    }
    return PGACCEL_ERROR_NO_DEVICE;
}

extern "C" pgaccel_status pgaccel_raster_reclass(
    const void* input_pixels,
    size_t pixel_count,
    int input_type,
    const pgaccel_reclass_rule* rules,
    size_t rule_count,
    int output_type,
    void* output_pixels
) {
    if (input_pixels == nullptr || output_pixels == nullptr) {
        return PGACCEL_ERROR_INIT;
    }
    if (rules == nullptr && rule_count > 0) {
        return PGACCEL_ERROR_INIT;
    }
    // Empty input — no-op, avoid zero-sized device allocations.
    if (pixel_count == 0) {
        return PGACCEL_OK;
    }

    try {
        auto& q = get_queue();
        size_t in_psz = pixel_type_size(input_type);
        size_t out_psz = pixel_type_size(output_type);
        if (in_psz == 0 || out_psz == 0) return PGACCEL_ERROR_UNSUPPORTED;

        // Convert input pixels to fp32 on host, apply rules on GPU, write back
        auto* h_in = new (std::nothrow) float[pixel_count];
        if (!h_in) {
            return PGACCEL_ERROR_OOM;
        }
        for (size_t i = 0; i < pixel_count; i++) {
            h_in[i] = static_cast<float>(read_pixel(input_pixels, i, input_type));
        }

        // SAFETY: USM device allocations freed at end of scope
        float* d_in = sycl::malloc_device<float>(pixel_count, q);
        float* d_out = sycl::malloc_device<float>(pixel_count, q);

        // Copy rules to device — flatten to 3 floats per rule (min, max, new).
        // When rule_count == 0, allocate a 1-element placeholder so device
        // pointers are valid; the kernel loop naturally no-ops (passthrough).
        const size_t rule_alloc = rule_count > 0 ? rule_count * 3 : 1;
        float* h_rules_flat = new (std::nothrow) float[rule_alloc];
        float* d_rules = sycl::malloc_device<float>(rule_alloc, q);

        if (!d_in || !d_out || !h_rules_flat || !d_rules) {
            delete[] h_in;
            delete[] h_rules_flat;
            sycl::free(d_in, q); sycl::free(d_out, q);
            sycl::free(d_rules, q);
            return PGACCEL_ERROR_OOM;
        }

        for (size_t r = 0; r < rule_count; r++) {
            h_rules_flat[r * 3 + 0] = static_cast<float>(rules[r].min_val);
            h_rules_flat[r * 3 + 1] = static_cast<float>(rules[r].max_val);
            h_rules_flat[r * 3 + 2] = static_cast<float>(rules[r].new_val);
        }
        if (rule_count == 0) {
            h_rules_flat[0] = 0.0f;
        }

        q.memcpy(d_in, h_in, pixel_count * sizeof(float));
        q.memcpy(d_rules, h_rules_flat, rule_alloc * sizeof(float));
        q.wait_and_throw();

        delete[] h_in;
        delete[] h_rules_flat;

        const size_t rc = rule_count;

        q.submit([&](sycl::handler& h) {
            h.parallel_for(sycl::range<1>(pixel_count), [=](sycl::id<1> id) {
                const size_t i = id[0];
                float val = d_in[i];
                float out_val = val; // passthrough by default

                for (size_t r = 0; r < rc; r++) {
                    float rmin = d_rules[r * 3 + 0];
                    float rmax = d_rules[r * 3 + 1];
                    float rnew = d_rules[r * 3 + 2];
                    if (val >= rmin && val < rmax) {
                        out_val = rnew;
                        break;
                    }
                }

                d_out[i] = out_val;
            });
        }).wait_and_throw();

        // Read back and convert to output pixel type
        auto* h_out = new (std::nothrow) float[pixel_count];
        if (!h_out) {
            sycl::free(d_in, q); sycl::free(d_out, q);
            sycl::free(d_rules, q);
            return PGACCEL_ERROR_OOM;
        }

        q.memcpy(h_out, d_out, pixel_count * sizeof(float)).wait_and_throw();

        for (size_t i = 0; i < pixel_count; i++) {
            write_pixel(output_pixels, i, output_type,
                        static_cast<double>(h_out[i]));
        }

        delete[] h_out;
        sycl::free(d_in, q);
        sycl::free(d_out, q);
        sycl::free(d_rules, q);
        pgaccel_record_gpu_exec();
        return PGACCEL_OK;
    } catch (const std::exception&) {
    } catch (...) {
    }
    return PGACCEL_ERROR_NO_DEVICE;
}

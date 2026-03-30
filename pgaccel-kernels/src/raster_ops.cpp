#include "pgaccel_ffi.h"

#include <cmath>
#include <cstring>

#if PGACCEL_HAS_SYCL
#include <sycl/sycl.hpp>
#endif

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

/* ── Point-in-ring (ray casting) ──────────────────────────────── */

static bool point_in_ring(
    double px, double py,
    const float* ring_xy, size_t vertex_count
) {
    bool inside = false;
    size_t j = vertex_count - 1;
    for (size_t i = 0; i < vertex_count; i++) {
        double xi = static_cast<double>(ring_xy[i * 2]);
        double yi = static_cast<double>(ring_xy[i * 2 + 1]);
        double xj = static_cast<double>(ring_xy[j * 2]);
        double yj = static_cast<double>(ring_xy[j * 2 + 1]);

        if (((yi > py) != (yj > py)) &&
            (px < (xj - xi) * (py - yi) / (yj - yi) + xi)) {
            inside = !inside;
        }
        j = i;
    }
    return inside;
}

/* ── CPU fallback: Map Algebra ────────────────────────────────── */

static pgaccel_status map_algebra_cpu(
    const void* const* band_pixels,
    size_t pixel_count,
    int pixel_type,
    const pgaccel_expr* expr,
    void* output_pixels,
    uint8_t* nodata_mask
) {
    if (pixel_count == 0) return PGACCEL_OK;
    if (pixel_type_size(pixel_type) == 0) return PGACCEL_ERROR_UNSUPPORTED;

    double band_values[64]; // max 64 bands

    for (size_t px = 0; px < pixel_count; px++) {
        // Skip NODATA pixels
        if (nodata_mask != nullptr && nodata_mask[px] != 0) {
            // Copy zero to output for NODATA pixels
            write_pixel(output_pixels, px, pixel_type, 0.0);
            continue;
        }

        // Load band values for this pixel
        for (size_t b = 0; b < expr->band_count; b++) {
            band_values[b] = read_pixel(band_pixels[b], px, pixel_type);
        }

        double result = eval_expr(expr, band_values);

        // If result is NaN, mark as NODATA
        if (std::isnan(result)) {
            if (nodata_mask != nullptr) {
                nodata_mask[px] = 1;
            }
            write_pixel(output_pixels, px, pixel_type, 0.0);
        } else {
            write_pixel(output_pixels, px, pixel_type, result);
        }
    }
    return PGACCEL_OK;
}

/* ── CPU fallback: Raster Clip ────────────────────────────────── */

static pgaccel_status raster_clip_cpu(
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
    size_t total = width * height;
    if (total == 0) return PGACCEL_OK;
    if (pixel_type_size(pixel_type) == 0) return PGACCEL_ERROR_UNSUPPORTED;

    size_t psz = pixel_type_size(pixel_type);

    for (size_t row = 0; row < height; row++) {
        for (size_t col = 0; col < width; col++) {
            size_t idx = row * width + col;

            // Compute pixel center in world coordinates
            double px = origin_x + (static_cast<double>(col) + 0.5) * scale_x;
            double py = origin_y + (static_cast<double>(row) + 0.5) * scale_y;

            // Copy pixel data
            std::memcpy(
                static_cast<char*>(output_pixels) + idx * psz,
                static_cast<const char*>(rast_pixels) + idx * psz,
                psz
            );

            if (point_in_ring(px, py, clip_ring_xy, vertex_count)) {
                if (nodata_mask != nullptr) {
                    nodata_mask[idx] = 0;
                }
            } else {
                if (nodata_mask != nullptr) {
                    nodata_mask[idx] = 1;
                }
            }
        }
    }
    return PGACCEL_OK;
}

/* ── CPU fallback: Raster Reclass ─────────────────────────────── */

static pgaccel_status raster_reclass_cpu(
    const void* input_pixels,
    size_t pixel_count,
    int input_type,
    const pgaccel_reclass_rule* rules,
    size_t rule_count,
    int output_type,
    void* output_pixels
) {
    if (pixel_count == 0) return PGACCEL_OK;
    if (pixel_type_size(input_type) == 0) return PGACCEL_ERROR_UNSUPPORTED;
    if (pixel_type_size(output_type) == 0) return PGACCEL_ERROR_UNSUPPORTED;

    for (size_t px = 0; px < pixel_count; px++) {
        double val = read_pixel(input_pixels, px, input_type);
        double out_val = val; // passthrough by default

        for (size_t r = 0; r < rule_count; r++) {
            if (val >= rules[r].min_val && val < rules[r].max_val) {
                out_val = rules[r].new_val;
                break;
            }
        }

        write_pixel(output_pixels, px, output_type, out_val);
    }
    return PGACCEL_OK;
}

/* ── SYCL GPU implementations ────────────────────────────────── */

#if PGACCEL_HAS_SYCL

static sycl::queue& get_queue() {
    static sycl::queue q{sycl::default_selector_v};
    return q;
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

    auto& q = get_queue();
    size_t psz = pixel_type_size(pixel_type);
    if (psz == 0) return PGACCEL_ERROR_UNSUPPORTED;

    // Allocate device buffers for band data (as double)
    size_t band_count = expr->band_count;
    double** host_band_doubles = new (std::nothrow) double*[band_count];
    if (!host_band_doubles) return PGACCEL_ERROR_OOM;

    for (size_t b = 0; b < band_count; b++) {
        host_band_doubles[b] = new (std::nothrow) double[pixel_count];
        if (!host_band_doubles[b]) {
            for (size_t j = 0; j < b; j++) delete[] host_band_doubles[j];
            delete[] host_band_doubles;
            return PGACCEL_ERROR_OOM;
        }
        for (size_t p = 0; p < pixel_count; p++) {
            host_band_doubles[b][p] = read_pixel(band_pixels[b], p, pixel_type);
        }
    }

    // For now, fall back to CPU eval with pre-converted doubles
    double band_values[64];
    for (size_t px = 0; px < pixel_count; px++) {
        if (nodata_mask != nullptr && nodata_mask[px] != 0) {
            write_pixel(output_pixels, px, pixel_type, 0.0);
            continue;
        }
        for (size_t b = 0; b < band_count; b++) {
            band_values[b] = host_band_doubles[b][px];
        }
        double result = eval_expr(expr, band_values);
        if (std::isnan(result)) {
            if (nodata_mask != nullptr) nodata_mask[px] = 1;
            write_pixel(output_pixels, px, pixel_type, 0.0);
        } else {
            write_pixel(output_pixels, px, pixel_type, result);
        }
    }

    for (size_t b = 0; b < band_count; b++) delete[] host_band_doubles[b];
    delete[] host_band_doubles;
    return PGACCEL_OK;
}

#endif /* PGACCEL_HAS_SYCL */

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

#if PGACCEL_HAS_SYCL
    return map_algebra_gpu(
        band_pixels, pixel_count, pixel_type, expr, output_pixels, nodata_mask);
#else
    return map_algebra_cpu(
        band_pixels, pixel_count, pixel_type, expr, output_pixels, nodata_mask);
#endif
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

#if PGACCEL_HAS_SYCL
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
            return raster_clip_cpu(
                rast_pixels, width, height,
                origin_x, origin_y, scale_x, scale_y,
                pixel_type, clip_ring_xy, vertex_count,
                output_pixels, nodata_mask);
        }

        q.memcpy(d_rast, rast_pixels, total * psz);
        q.memcpy(d_ring, clip_ring_xy, vertex_count * 2 * sizeof(float));
        if (nodata_mask) {
            q.memcpy(d_mask, nodata_mask, total * sizeof(uint8_t));
        } else {
            q.memset(d_mask, 0, total * sizeof(uint8_t));
        }
        q.wait();

        // Copy raster pixels to output buffer on device
        q.memcpy(d_out, d_rast, total * psz).wait();

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
        }).wait();

        q.memcpy(output_pixels, d_out, total * psz);
        q.memcpy(nodata_mask, d_mask, total * sizeof(uint8_t));
        q.wait();

        sycl::free(d_rast, q);
        sycl::free(d_out, q);
        sycl::free(d_mask, q);
        sycl::free(d_ring, q);
        return PGACCEL_OK;
    } catch (const sycl::exception&) {
        // SYCL unavailable at runtime, fall through to CPU
    }
#endif
    return raster_clip_cpu(
        rast_pixels, width, height,
        origin_x, origin_y, scale_x, scale_y,
        pixel_type, clip_ring_xy, vertex_count,
        output_pixels, nodata_mask);
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

#if PGACCEL_HAS_SYCL
    try {
        auto& q = get_queue();
        size_t in_psz = pixel_type_size(input_type);
        size_t out_psz = pixel_type_size(output_type);
        if (in_psz == 0 || out_psz == 0) return PGACCEL_ERROR_UNSUPPORTED;

        // Convert input pixels to fp32 on host, apply rules on GPU, write back
        auto* h_in = new (std::nothrow) float[pixel_count];
        if (!h_in) {
            return raster_reclass_cpu(
                input_pixels, pixel_count, input_type,
                rules, rule_count, output_type, output_pixels);
        }
        for (size_t i = 0; i < pixel_count; i++) {
            h_in[i] = static_cast<float>(read_pixel(input_pixels, i, input_type));
        }

        // SAFETY: USM device allocations freed at end of scope
        float* d_in = sycl::malloc_device<float>(pixel_count, q);
        float* d_out = sycl::malloc_device<float>(pixel_count, q);

        // Copy rules to device — flatten to 3 floats per rule (min, max, new)
        float* h_rules_flat = new (std::nothrow) float[rule_count * 3];
        float* d_rules = sycl::malloc_device<float>(rule_count * 3, q);

        if (!d_in || !d_out || !h_rules_flat || !d_rules) {
            delete[] h_in;
            delete[] h_rules_flat;
            sycl::free(d_in, q); sycl::free(d_out, q);
            sycl::free(d_rules, q);
            return raster_reclass_cpu(
                input_pixels, pixel_count, input_type,
                rules, rule_count, output_type, output_pixels);
        }

        for (size_t r = 0; r < rule_count; r++) {
            h_rules_flat[r * 3 + 0] = static_cast<float>(rules[r].min_val);
            h_rules_flat[r * 3 + 1] = static_cast<float>(rules[r].max_val);
            h_rules_flat[r * 3 + 2] = static_cast<float>(rules[r].new_val);
        }

        q.memcpy(d_in, h_in, pixel_count * sizeof(float));
        q.memcpy(d_rules, h_rules_flat, rule_count * 3 * sizeof(float));
        q.wait();

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
        }).wait();

        // Read back and convert to output pixel type
        auto* h_out = new (std::nothrow) float[pixel_count];
        if (!h_out) {
            sycl::free(d_in, q); sycl::free(d_out, q);
            sycl::free(d_rules, q);
            return raster_reclass_cpu(
                input_pixels, pixel_count, input_type,
                rules, rule_count, output_type, output_pixels);
        }

        q.memcpy(h_out, d_out, pixel_count * sizeof(float)).wait();

        for (size_t i = 0; i < pixel_count; i++) {
            write_pixel(output_pixels, i, output_type,
                        static_cast<double>(h_out[i]));
        }

        delete[] h_out;
        sycl::free(d_in, q);
        sycl::free(d_out, q);
        sycl::free(d_rules, q);
        return PGACCEL_OK;
    } catch (const sycl::exception&) {
        // SYCL unavailable at runtime, fall through to CPU
    }
#endif
    return raster_reclass_cpu(
        input_pixels, pixel_count, input_type,
        rules, rule_count, output_type, output_pixels);
}

#include <sycl/sycl.hpp>

#include <cmath>
#include <cstdint>
#include <cstdio>
#include <cstring>
#include <limits>
#include <new>
#include <stdexcept>
#include <vector>

#include "pgaccel_ffi.h"
#include "pgaccel_queue.h"

// SAFETY: g_queue is owned by device_manager.cpp. Raster kernels must share
// the same process-global queue so pgaccel_shutdown() can release the Metal
// context; a private static queue leaks an extra runtime context at exit.

/* ── Pixel type helpers ───────────────────────────────────────── */

static size_t pixel_type_size(int pt) {
  switch (static_cast<pgaccel_pixel_type>(pt)) {
    case PGACCEL_PT_INT8:
      return 1;
    case PGACCEL_PT_INT16:
      return 2;
    case PGACCEL_PT_INT32:
      return 4;
    case PGACCEL_PT_FLOAT32:
      return 4;
    case PGACCEL_PT_FLOAT64:
      return 8;
  }
  return 0;
}

static double read_pixel(const void* data, size_t idx, int pt) {
  switch (static_cast<pgaccel_pixel_type>(pt)) {
    case PGACCEL_PT_INT8:
      return static_cast<double>(static_cast<const int8_t*>(data)[idx]);
    case PGACCEL_PT_INT16:
      return static_cast<double>(static_cast<const int16_t*>(data)[idx]);
    case PGACCEL_PT_INT32:
      return static_cast<double>(static_cast<const int32_t*>(data)[idx]);
    case PGACCEL_PT_FLOAT32:
      return static_cast<double>(static_cast<const float*>(data)[idx]);
    case PGACCEL_PT_FLOAT64:
      return static_cast<const double*>(data)[idx];
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

/* ── SYCL GPU implementations ────────────────────────────────── */
//
// CLAUDE.md rules 11/12 — kernels execute on GPU via SYCL. The
// host-side `eval_expr` interpreter and `eval_expr_f32_inline`
// scalar fast-path that previously ran small tiles on CPU (and
// fraudulently called `pgaccel_record_gpu_exec()` afterwards) were
// deleted in the 2026-05-02 cheat audit. The real device-side
// bytecode evaluator lives inside `map_algebra_gpu`'s parallel_for
// kernel body.

static sycl::queue& get_queue() {
  return pgaccel_require_queue();
}

// The pgaccel_expr_inst.arg union is written by Rust through a single
// f64-sized mirror field: LOAD_BAND stores the band index via
// f64::from_bits(index as u64) (pg_accel/src/engine/dispatch/raster.rs
// load_band_inst), and the device code below reads it back through the
// union's `int band_index` member. That aliasing is only correct when the
// i32 occupies the low-order bytes of the 8-byte slot — i.e. on
// little-endian targets. Pin it so a big-endian port fails at compile time
// instead of silently reading garbage band indices.
static_assert(__BYTE_ORDER__ == __ORDER_LITTLE_ENDIAN__,
              "pgaccel raster band-index union punning requires a little-endian target");

static constexpr size_t PGACCEL_MAP_ALGEBRA_MAX_BANDS = 8;
static constexpr int PGACCEL_MAP_ALGEBRA_MAX_STACK = 16;

static pgaccel_status validate_map_algebra_expr(const void* const* band_pixels,
                                                const pgaccel_expr* expr) {
  if (expr->inst_count == 0) {
    return PGACCEL_ERROR;
  }
  if (expr->band_count == 0 || expr->band_count > PGACCEL_MAP_ALGEBRA_MAX_BANDS) {
    return PGACCEL_ERROR_UNSUPPORTED;
  }
  for (size_t b = 0; b < expr->band_count; ++b) {
    if (band_pixels[b] == nullptr) {
      return PGACCEL_ERROR_INIT;
    }
  }

  int depth = 0;
  for (size_t i = 0; i < expr->inst_count; ++i) {
    const pgaccel_expr_inst inst = expr->instructions[i];
    switch (inst.op) {
      case PGACCEL_OP_LOAD_BAND:
        if (inst.arg.band_index < 0 ||
            static_cast<size_t>(inst.arg.band_index) >= expr->band_count) {
          return PGACCEL_ERROR_UNSUPPORTED;
        }
        ++depth;
        break;
      case PGACCEL_OP_LOAD_CONST:
        ++depth;
        break;
      case PGACCEL_OP_SQRT:
      case PGACCEL_OP_ABS:
      case PGACCEL_OP_LOG:
        if (depth < 1) {
          return PGACCEL_ERROR_UNSUPPORTED;
        }
        break;
      case PGACCEL_OP_ADD:
      case PGACCEL_OP_SUB:
      case PGACCEL_OP_MUL:
      case PGACCEL_OP_DIV:
      case PGACCEL_OP_POW:
      case PGACCEL_OP_GT:
      case PGACCEL_OP_LT:
      case PGACCEL_OP_EQ:
        if (depth < 2) {
          return PGACCEL_ERROR_UNSUPPORTED;
        }
        --depth;
        break;
      case PGACCEL_OP_SELECT:
        if (depth < 3) {
          return PGACCEL_ERROR_UNSUPPORTED;
        }
        depth -= 2;
        break;
      default:
        return PGACCEL_ERROR_UNSUPPORTED;
    }

    if (depth > PGACCEL_MAP_ALGEBRA_MAX_STACK) {
      return PGACCEL_ERROR_UNSUPPORTED;
    }
  }

  return depth == 1 ? PGACCEL_OK : PGACCEL_ERROR_UNSUPPORTED;
}

/* map_algebra GPU dispatcher.
 *
 * Per CLAUDE.md rules 11/12 there is no host-loop fast-path or
 * non-FP32 host fallback: every pixel goes through the SYCL kernel.
 * The previous small-tile inline evaluator and the post-SYCL host
 * fallthrough loop both called pgaccel_record_gpu_exec() while
 * computing on CPU — that fraudulent stats reporting is gone.
 *
 * Only PGACCEL_PT_FLOAT32 inputs are accelerated today (Metal is
 * fp32-only, and the kernel body uses float throughout). Other pixel
 * types return PGACCEL_ERROR_UNSUPPORTED so the caller routes through
 * PG (the documented escape hatch for unsupported input shapes).
 */

static pgaccel_status map_algebra_gpu(const void* const* band_pixels, size_t pixel_count,
                                      int pixel_type, const pgaccel_expr* expr, void* output_pixels,
                                      uint8_t* nodata_mask) {
  if (pixel_count == 0)
    return PGACCEL_OK;

  size_t psz = pixel_type_size(pixel_type);
  if (psz == 0)
    return PGACCEL_ERROR_UNSUPPORTED;

  // FP32 is the only pixel type the kernel handles. Non-FP32 inputs
  // are explicitly declined (caller falls back to PG via the standard
  // unsupported-input route, NOT a silent CPU compute path).
  if (pixel_type != PGACCEL_PT_FLOAT32) {
    return PGACCEL_ERROR_UNSUPPORTED;
  }

  const pgaccel_status validation = validate_map_algebra_expr(band_pixels, expr);
  if (validation != PGACCEL_OK) {
    return validation;
  }

  size_t band_count = expr->band_count;

  // Real GPU path — single dispatch through SYCL parallel_for.
  {
    try {
      auto& q = get_queue();

      // Copy instructions to device (or shared).
      const size_t n_inst = expr->inst_count;
      pgaccel_expr_inst* d_inst =
          sycl::malloc_shared<pgaccel_expr_inst>(n_inst > 0 ? n_inst : 1, q);
      if (!d_inst)
        return PGACCEL_ERROR_OOM;
      std::memcpy(d_inst, expr->instructions, n_inst * sizeof(pgaccel_expr_inst));

      constexpr size_t MAX_BANDS = PGACCEL_MAP_ALGEBRA_MAX_BANDS;
      if (band_count > MAX_BANDS || pixel_count > SIZE_MAX / band_count) {
        sycl::free(d_inst, q);
        return PGACCEL_ERROR_UNSUPPORTED;
      }

      // Store all bands in one flat device allocation. This keeps the
      // map_algebra Metal closure to a single band data pointer instead
      // of capturing a struct of up to eight device pointers.
      const size_t band_stride = pixel_count;
      float* d_bands = sycl::malloc_device<float>(band_count * band_stride, q);
      if (!d_bands) {
        sycl::free(d_inst, q);
        return PGACCEL_ERROR_OOM;
      }
      for (size_t b = 0; b < band_count; ++b) {
        q.memcpy(d_bands + b * band_stride, band_pixels[b], pixel_count * sizeof(float));
      }
      float* d_out = sycl::malloc_device<float>(pixel_count, q);
      uint8_t* d_mask = sycl::malloc_device<uint8_t>(pixel_count, q);
      if (!d_out || !d_mask) {
        sycl::free(d_bands, q);
        if (d_out)
          sycl::free(d_out, q);
        if (d_mask)
          sycl::free(d_mask, q);
        sycl::free(d_inst, q);
        return PGACCEL_ERROR_OOM;
      }
      if (nodata_mask) {
        q.memcpy(d_mask, nodata_mask, pixel_count);
      } else {
        q.memset(d_mask, 0, pixel_count);
      }
      q.wait_and_throw();

      const size_t n = pixel_count;
      const size_t ni = n_inst;
      const size_t bc = band_count;
      const size_t bs = band_stride;

      // Metal is fp32-only — kernel body uses float throughout.
      const float nan_f = sycl::bit_cast<float>(static_cast<uint32_t>(0x7fc00000u));
      q.submit([&](sycl::handler& h) {
         h.parallel_for(sycl::range<1>(n), [=](sycl::id<1> id) {
           const size_t i = id[0];
           if (d_mask[i] != 0) {
             d_out[i] = 0.0f;
             return;
           }
           float band_vals[MAX_BANDS];
           for (size_t b = 0; b < bc; ++b) {
             band_vals[b] = d_bands[b * bs + i];
           }
           float stack_vals[16];
           int sp = 0;
           for (size_t j = 0; j < ni; ++j) {
             const pgaccel_expr_inst inst = d_inst[j];
             switch (inst.op) {
               case PGACCEL_OP_LOAD_BAND:
                 stack_vals[sp++] = band_vals[inst.arg.band_index];
                 break;
               case PGACCEL_OP_LOAD_CONST:
                 stack_vals[sp++] = static_cast<float>(inst.arg.constant);
                 break;
               case PGACCEL_OP_ADD: {
                 float bv = stack_vals[--sp];
                 stack_vals[sp - 1] += bv;
                 break;
               }
               case PGACCEL_OP_SUB: {
                 float bv = stack_vals[--sp];
                 stack_vals[sp - 1] -= bv;
                 break;
               }
               case PGACCEL_OP_MUL: {
                 float bv = stack_vals[--sp];
                 stack_vals[sp - 1] *= bv;
                 break;
               }
               case PGACCEL_OP_DIV: {
                 float bv = stack_vals[--sp];
                 stack_vals[sp - 1] = (bv == 0.0f) ? nan_f : stack_vals[sp - 1] / bv;
                 break;
               }
               case PGACCEL_OP_SQRT:
                 stack_vals[sp - 1] = sycl::sqrt(stack_vals[sp - 1]);
                 break;
               case PGACCEL_OP_ABS:
                 stack_vals[sp - 1] = sycl::fabs(stack_vals[sp - 1]);
                 break;
               case PGACCEL_OP_LOG:
                 stack_vals[sp - 1] =
                     (stack_vals[sp - 1] > 0.0f) ? sycl::log(stack_vals[sp - 1]) : nan_f;
                 break;
               case PGACCEL_OP_POW: {
                 float bv = stack_vals[--sp];
                 stack_vals[sp - 1] = sycl::pow(stack_vals[sp - 1], bv);
                 break;
               }
               case PGACCEL_OP_GT: {
                 float bv = stack_vals[--sp];
                 stack_vals[sp - 1] = (stack_vals[sp - 1] > bv) ? 1.0f : 0.0f;
                 break;
               }
               case PGACCEL_OP_LT: {
                 float bv = stack_vals[--sp];
                 stack_vals[sp - 1] = (stack_vals[sp - 1] < bv) ? 1.0f : 0.0f;
                 break;
               }
               case PGACCEL_OP_EQ: {
                 float bv = stack_vals[--sp];
                 stack_vals[sp - 1] = (stack_vals[sp - 1] == bv) ? 1.0f : 0.0f;
                 break;
               }
               case PGACCEL_OP_SELECT: {
                 float fb = stack_vals[--sp];
                 float tb = stack_vals[--sp];
                 float c = stack_vals[--sp];
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

      sycl::free(d_bands, q);
      sycl::free(d_out, q);
      sycl::free(d_mask, q);
      sycl::free(d_inst, q);

      pgaccel_record_gpu_exec();
      return PGACCEL_OK;
    } catch (const std::exception& e) {
      fprintf(stderr, "pgaccel: SYCL map_algebra failed: %s\n", e.what());
      // Surface the kernel failure to the caller so the planner / executor
      // can route to PG instead of silently miscomputing. Suppress the stats counter so
      // EXPLAIN ANALYZE doesn't credit a failed dispatch.
      return PGACCEL_ERROR;
    }
  }
}

/* ── Public API ───────────────────────────────────────────────── */

extern "C" pgaccel_status pgaccel_map_algebra(const void* const* band_pixels, size_t pixel_count,
                                              int pixel_type, const pgaccel_expr* expr,
                                              void* output_pixels, uint8_t* nodata_mask) try {
  if (band_pixels == nullptr || expr == nullptr || output_pixels == nullptr) {
    return PGACCEL_ERROR_INIT;
  }
  if (expr->instructions == nullptr && expr->inst_count > 0) {
    return PGACCEL_ERROR_INIT;
  }

  return map_algebra_gpu(band_pixels, pixel_count, pixel_type, expr, output_pixels, nodata_mask);
} catch (const pgaccel_no_device_error&) {
  return PGACCEL_ERROR_NO_DEVICE;
} catch (const std::exception& e) {
  return pgaccel_kernel_failure("pgaccel_map_algebra", &e);
} catch (...) {
  return pgaccel_kernel_failure("pgaccel_map_algebra", nullptr);
}

extern "C" pgaccel_status pgaccel_raster_clip(const void* rast_pixels, size_t width, size_t height,
                                              double origin_x, double origin_y, double scale_x,
                                              double scale_y, int pixel_type,
                                              const float* clip_ring_xy, size_t vertex_count,
                                              void* output_pixels, uint8_t* nodata_mask) try {
  if (rast_pixels == nullptr || clip_ring_xy == nullptr || output_pixels == nullptr ||
      nodata_mask == nullptr) {
    return PGACCEL_ERROR_INIT;
  }

  // Empty raster — no-op, avoid zero-sized device allocations.
  if (width == 0 || height == 0) {
    return PGACCEL_OK;
  }

  // No host fast-path: the previous small-tile branch called
  // raster_clip_inline_f32 (CPU loop) AND pgaccel_record_gpu_exec()
  // afterwards, fraudulently crediting the GPU stats counter for CPU
  // work. CLAUDE.md rules 11/12 — every dispatch goes through SYCL.
  // Per-batch dispatch latency is the Phase 6 problem, not a problem
  // to bypass via cheating here.

  try {
    auto& q = get_queue();
    size_t total = width * height;
    size_t psz = pixel_type_size(pixel_type);
    if (psz == 0)
      return PGACCEL_ERROR_UNSUPPORTED;

    // SAFETY: USM device allocations freed at end of scope
    char* d_rast = static_cast<char*>(sycl::malloc_device(total * psz, q));
    char* d_out = static_cast<char*>(sycl::malloc_device(total * psz, q));
    uint8_t* d_mask = sycl::malloc_device<uint8_t>(total, q);
    float* d_ring = sycl::malloc_device<float>(vertex_count * 2, q);

    if (!d_rast || !d_out || !d_mask || !d_ring) {
      sycl::free(d_rast, q);
      sycl::free(d_out, q);
      sycl::free(d_mask, q);
      sycl::free(d_ring, q);
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

           if (((yi > py) != (yj > py)) && (px < (xj - xi) * (py - yi) / (yj - yi) + xi)) {
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
  } catch (const pgaccel_no_device_error&) {
    return PGACCEL_ERROR_NO_DEVICE;
  } catch (const std::exception& e) {
    return pgaccel_kernel_failure(__func__, &e);
  } catch (...) {
    return pgaccel_kernel_failure(__func__, nullptr);
  }
  return PGACCEL_ERROR_NO_DEVICE;
} catch (const pgaccel_no_device_error&) {
  return PGACCEL_ERROR_NO_DEVICE;
} catch (const std::exception& e) {
  return pgaccel_kernel_failure("pgaccel_raster_clip", &e);
} catch (...) {
  return pgaccel_kernel_failure("pgaccel_raster_clip", nullptr);
}

/* ── Raster Resample (bilinear interpolation) ─────────────────── */
/*
 * Bilinear-interpolate src_pixels (W×H, fp32) to dst_pixels (new_W×new_H,
 * fp32). Scale factors are derived per-axis (src/dst). Out-of-range
 * neighbours clamp to nearest edge. Input/output pixel type is FP32 only
 * — Metal is fp32-only and the interpolation kernel uses float
 * throughout (CLAUDE.md rule 12).
 */
extern "C" pgaccel_status pgaccel_raster_resample(const float* src_pixels, size_t src_w,
                                                  size_t src_h, size_t dst_w, size_t dst_h,
                                                  float* dst_pixels) try {
  if (src_pixels == nullptr || dst_pixels == nullptr) {
    return PGACCEL_ERROR_INIT;
  }
  if (src_w == 0 || src_h == 0 || dst_w == 0 || dst_h == 0) {
    return PGACCEL_OK;
  }

  try {
    auto& q = get_queue();
    const size_t src_n = src_w * src_h;
    const size_t dst_n = dst_w * dst_h;

    // SAFETY: USM device allocations freed at end of scope.
    float* d_src = sycl::malloc_device<float>(src_n, q);
    float* d_dst = sycl::malloc_device<float>(dst_n, q);
    if (!d_src || !d_dst) {
      sycl::free(d_src, q);
      sycl::free(d_dst, q);
      return PGACCEL_ERROR_OOM;
    }

    q.memcpy(d_src, src_pixels, src_n * sizeof(float)).wait_and_throw();

    const float sw = static_cast<float>(src_w);
    const float sh = static_cast<float>(src_h);
    const float dw = static_cast<float>(dst_w);
    const float dh = static_cast<float>(dst_h);
    const size_t sw_sz = src_w;
    const size_t sh_sz = src_h;
    const size_t dw_sz = dst_w;

    q.submit([&](sycl::handler& h) {
       h.parallel_for(sycl::range<1>(dst_n), [=](sycl::id<1> id) {
         const size_t idx = id[0];
         const size_t dr = idx / dw_sz;
         const size_t dc = idx - dr * dw_sz;

         // Map dst pixel center to src coordinate space (pixel center at
         // (dc + 0.5) / dw, scaled to src by * sw - 0.5 to align centres).
         float sx = (static_cast<float>(dc) + 0.5f) * sw / dw - 0.5f;
         float sy = (static_cast<float>(dr) + 0.5f) * sh / dh - 0.5f;

         // Clamp to valid neighbour range [0, n-1].
         if (sx < 0.0f)
           sx = 0.0f;
         if (sy < 0.0f)
           sy = 0.0f;
         const float maxx = sw - 1.0f;
         const float maxy = sh - 1.0f;
         if (sx > maxx)
           sx = maxx;
         if (sy > maxy)
           sy = maxy;

         const size_t x0 = static_cast<size_t>(sycl::floor(sx));
         const size_t y0 = static_cast<size_t>(sycl::floor(sy));
         size_t x1 = x0 + 1;
         size_t y1 = y0 + 1;
         if (x1 >= sw_sz)
           x1 = sw_sz - 1;
         if (y1 >= sh_sz)
           y1 = sh_sz - 1;

         const float fx = sx - static_cast<float>(x0);
         const float fy = sy - static_cast<float>(y0);

         const float p00 = d_src[y0 * sw_sz + x0];
         const float p10 = d_src[y0 * sw_sz + x1];
         const float p01 = d_src[y1 * sw_sz + x0];
         const float p11 = d_src[y1 * sw_sz + x1];

         const float top = p00 * (1.0f - fx) + p10 * fx;
         const float bot = p01 * (1.0f - fx) + p11 * fx;
         d_dst[idx] = top * (1.0f - fy) + bot * fy;
       });
     }).wait_and_throw();

    q.memcpy(dst_pixels, d_dst, dst_n * sizeof(float)).wait_and_throw();

    sycl::free(d_src, q);
    sycl::free(d_dst, q);
    pgaccel_record_gpu_exec();
    return PGACCEL_OK;
  } catch (const std::exception& e) {
    fprintf(stderr, "pgaccel: SYCL raster_resample failed: %s\n", e.what());
    return PGACCEL_ERROR;
  }
} catch (const pgaccel_no_device_error&) {
  return PGACCEL_ERROR_NO_DEVICE;
} catch (const std::exception& e) {
  return pgaccel_kernel_failure("pgaccel_raster_resample", &e);
} catch (...) {
  return pgaccel_kernel_failure("pgaccel_raster_resample", nullptr);
}

/* ── Raster Slope (Horn's method) ─────────────────────────────── */
/*
 * Per-pixel slope angle in degrees, computed via 3×3 Horn gradient. Edge
 * pixels (border 1-pixel ring) get slope 0 — the 3×3 stencil is undefined
 * there. cell_size_x / cell_size_y are world units per pixel (e.g. metres
 * for projected rasters). Output is fp32 degrees in [0, 90].
 */
extern "C" pgaccel_status pgaccel_raster_slope(const float* src_pixels, size_t width, size_t height,
                                               double cell_size_x, double cell_size_y,
                                               float* slope_out) try {
  if (src_pixels == nullptr || slope_out == nullptr) {
    return PGACCEL_ERROR_INIT;
  }
  if (width == 0 || height == 0) {
    return PGACCEL_OK;
  }

  try {
    auto& q = get_queue();
    const size_t n = width * height;

    float* d_src = sycl::malloc_device<float>(n, q);
    float* d_out = sycl::malloc_device<float>(n, q);
    if (!d_src || !d_out) {
      sycl::free(d_src, q);
      sycl::free(d_out, q);
      return PGACCEL_ERROR_OOM;
    }

    q.memcpy(d_src, src_pixels, n * sizeof(float)).wait_and_throw();

    const size_t w = width;
    const size_t h_sz = height;
    const float csx = static_cast<float>(cell_size_x);
    const float csy = static_cast<float>(cell_size_y);

    q.submit([&](sycl::handler& h) {
       h.parallel_for(sycl::range<1>(n), [=](sycl::id<1> id) {
         const size_t idx = id[0];
         const size_t r = idx / w;
         const size_t c = idx - r * w;

         if (r == 0 || c == 0 || r >= h_sz - 1 || c >= w - 1) {
           d_out[idx] = 0.0f;
           return;
         }

         // Horn's 3×3 kernel:
         //   dz/dx = ((a + 2d + g) - (c + 2f + i)) / (8 * csx)
         //   dz/dy = ((g + 2h + i) - (a + 2b + c)) / (8 * csy)
         // Layout (a..i):
         //   a b c
         //   d e f
         //   g h i
         const size_t row_p = (r - 1) * w;
         const size_t row_0 = r * w;
         const size_t row_n = (r + 1) * w;
         const float a = d_src[row_p + c - 1];
         const float b = d_src[row_p + c];
         const float c_v = d_src[row_p + c + 1];
         const float d = d_src[row_0 + c - 1];
         const float f = d_src[row_0 + c + 1];
         const float g = d_src[row_n + c - 1];
         const float h_v = d_src[row_n + c];
         const float i = d_src[row_n + c + 1];

         const float dzdx = ((a + 2.0f * d + g) - (c_v + 2.0f * f + i)) / (8.0f * csx);
         const float dzdy = ((g + 2.0f * h_v + i) - (a + 2.0f * b + c_v)) / (8.0f * csy);
         const float rise_run = sycl::sqrt(dzdx * dzdx + dzdy * dzdy);
         const float slope_rad = sycl::atan(rise_run);
         d_out[idx] = slope_rad * (180.0f / 3.14159265358979323846f);
       });
     }).wait_and_throw();

    q.memcpy(slope_out, d_out, n * sizeof(float)).wait_and_throw();

    sycl::free(d_src, q);
    sycl::free(d_out, q);
    pgaccel_record_gpu_exec();
    return PGACCEL_OK;
  } catch (const std::exception& e) {
    fprintf(stderr, "pgaccel: SYCL raster_slope failed: %s\n", e.what());
    return PGACCEL_ERROR;
  }
} catch (const pgaccel_no_device_error&) {
  return PGACCEL_ERROR_NO_DEVICE;
} catch (const std::exception& e) {
  return pgaccel_kernel_failure("pgaccel_raster_slope", &e);
} catch (...) {
  return pgaccel_kernel_failure("pgaccel_raster_slope", nullptr);
}

/* ── Raster Aspect (3×3 gradient → compass direction) ─────────── */
/*
 * Per-pixel aspect (compass direction of the steepest descent), in degrees
 * [0, 360). North = 0, East = 90, South = 180, West = 270. Flat areas
 * (zero gradient) get -1 by convention (matching gdaldem). Edge pixels
 * are also -1.
 */
extern "C" pgaccel_status pgaccel_raster_aspect(const float* src_pixels, size_t width,
                                                size_t height, float* aspect_out) try {
  if (src_pixels == nullptr || aspect_out == nullptr) {
    return PGACCEL_ERROR_INIT;
  }
  if (width == 0 || height == 0) {
    return PGACCEL_OK;
  }

  try {
    auto& q = get_queue();
    const size_t n = width * height;

    float* d_src = sycl::malloc_device<float>(n, q);
    float* d_out = sycl::malloc_device<float>(n, q);
    if (!d_src || !d_out) {
      sycl::free(d_src, q);
      sycl::free(d_out, q);
      return PGACCEL_ERROR_OOM;
    }

    q.memcpy(d_src, src_pixels, n * sizeof(float)).wait_and_throw();

    const size_t w = width;
    const size_t h_sz = height;

    q.submit([&](sycl::handler& h) {
       h.parallel_for(sycl::range<1>(n), [=](sycl::id<1> id) {
         const size_t idx = id[0];
         const size_t r = idx / w;
         const size_t c = idx - r * w;

         if (r == 0 || c == 0 || r >= h_sz - 1 || c >= w - 1) {
           d_out[idx] = -1.0f;
           return;
         }

         const size_t row_p = (r - 1) * w;
         const size_t row_0 = r * w;
         const size_t row_n = (r + 1) * w;
         const float a = d_src[row_p + c - 1];
         const float b = d_src[row_p + c];
         const float c_v = d_src[row_p + c + 1];
         const float d = d_src[row_0 + c - 1];
         const float f = d_src[row_0 + c + 1];
         const float g = d_src[row_n + c - 1];
         const float h_v = d_src[row_n + c];
         const float i = d_src[row_n + c + 1];

         // Cell-size cancels in the atan2 ratio → use raw sums.
         const float dzdx = (c_v + 2.0f * f + i) - (a + 2.0f * d + g);
         const float dzdy = (g + 2.0f * h_v + i) - (a + 2.0f * b + c_v);

         if (dzdx == 0.0f && dzdy == 0.0f) {
           d_out[idx] = -1.0f;
           return;
         }

         // gdaldem aspect: 180 / pi * atan2(dzdy, -dzdx); then fold to compass.
         const float k_pi = 3.14159265358979323846f;
         float aspect = sycl::atan2(dzdy, -dzdx) * (180.0f / k_pi);
         if (aspect < 0.0f) {
           aspect = 90.0f - aspect;
         } else if (aspect > 90.0f) {
           aspect = 360.0f - aspect + 90.0f;
         } else {
           aspect = 90.0f - aspect;
         }
         if (aspect >= 360.0f)
           aspect -= 360.0f;
         d_out[idx] = aspect;
       });
     }).wait_and_throw();

    q.memcpy(aspect_out, d_out, n * sizeof(float)).wait_and_throw();

    sycl::free(d_src, q);
    sycl::free(d_out, q);
    pgaccel_record_gpu_exec();
    return PGACCEL_OK;
  } catch (const std::exception& e) {
    fprintf(stderr, "pgaccel: SYCL raster_aspect failed: %s\n", e.what());
    return PGACCEL_ERROR;
  }
} catch (const pgaccel_no_device_error&) {
  return PGACCEL_ERROR_NO_DEVICE;
} catch (const std::exception& e) {
  return pgaccel_kernel_failure("pgaccel_raster_aspect", &e);
} catch (...) {
  return pgaccel_kernel_failure("pgaccel_raster_aspect", nullptr);
}

/* ── Raster Hillshade (slope + aspect + sun) ──────────────────── */
/*
 * Per-pixel shaded relief value [0, 255]. Uses Horn's slope/aspect plus
 * sun azimuth (degrees clockwise from north) and altitude (degrees above
 * horizon). z_factor scales pixel value height units to match cell_size
 * units. Edge pixels get 0.
 */
extern "C" pgaccel_status pgaccel_raster_hillshade(const float* src_pixels, size_t width,
                                                   size_t height, double cell_size_x,
                                                   double cell_size_y, double sun_azimuth_deg,
                                                   double sun_altitude_deg, double z_factor,
                                                   float* shade_out) try {
  if (src_pixels == nullptr || shade_out == nullptr) {
    return PGACCEL_ERROR_INIT;
  }
  if (width == 0 || height == 0) {
    return PGACCEL_OK;
  }

  try {
    auto& q = get_queue();
    const size_t n = width * height;

    float* d_src = sycl::malloc_device<float>(n, q);
    float* d_out = sycl::malloc_device<float>(n, q);
    if (!d_src || !d_out) {
      sycl::free(d_src, q);
      sycl::free(d_out, q);
      return PGACCEL_ERROR_OOM;
    }

    q.memcpy(d_src, src_pixels, n * sizeof(float)).wait_and_throw();

    const size_t w = width;
    const size_t h_sz = height;
    const float csx = static_cast<float>(cell_size_x);
    const float csy = static_cast<float>(cell_size_y);
    const float zf = static_cast<float>(z_factor);
    const float k_pi = 3.14159265358979323846f;
    // Convert sun azimuth (compass deg, N=0 CW) to math angle (E=0 CCW)
    // and altitude to zenith.
    const float az_math_rad =
        (360.0f - static_cast<float>(sun_azimuth_deg) + 90.0f) * k_pi / 180.0f;
    const float zenith_rad = (90.0f - static_cast<float>(sun_altitude_deg)) * k_pi / 180.0f;

    q.submit([&](sycl::handler& h) {
       h.parallel_for(sycl::range<1>(n), [=](sycl::id<1> id) {
         const size_t idx = id[0];
         const size_t r = idx / w;
         const size_t c = idx - r * w;

         if (r == 0 || c == 0 || r >= h_sz - 1 || c >= w - 1) {
           d_out[idx] = 0.0f;
           return;
         }

         const size_t row_p = (r - 1) * w;
         const size_t row_0 = r * w;
         const size_t row_n = (r + 1) * w;
         const float a = d_src[row_p + c - 1];
         const float b = d_src[row_p + c];
         const float c_v = d_src[row_p + c + 1];
         const float d = d_src[row_0 + c - 1];
         const float f = d_src[row_0 + c + 1];
         const float g = d_src[row_n + c - 1];
         const float h_v = d_src[row_n + c];
         const float i = d_src[row_n + c + 1];

         const float dzdx = ((c_v + 2.0f * f + i) - (a + 2.0f * d + g)) * zf / (8.0f * csx);
         const float dzdy = ((g + 2.0f * h_v + i) - (a + 2.0f * b + c_v)) * zf / (8.0f * csy);

         const float slope_rad = sycl::atan(sycl::sqrt(dzdx * dzdx + dzdy * dzdy));
         float aspect_rad;
         if (dzdx == 0.0f && dzdy == 0.0f) {
           aspect_rad = 0.0f;
         } else {
           aspect_rad = sycl::atan2(dzdy, -dzdx);
           if (aspect_rad < 0.0f)
             aspect_rad += 2.0f * k_pi;
         }

         // Hillshade formula (gdaldem):
         //   shade = 255 * (cos(zen) * cos(slope) + sin(zen) * sin(slope) * cos(az - aspect))
         float shade =
             sycl::cos(zenith_rad) * sycl::cos(slope_rad) +
             sycl::sin(zenith_rad) * sycl::sin(slope_rad) * sycl::cos(az_math_rad - aspect_rad);
         if (shade < 0.0f)
           shade = 0.0f;
         d_out[idx] = shade * 255.0f;
       });
     }).wait_and_throw();

    q.memcpy(shade_out, d_out, n * sizeof(float)).wait_and_throw();

    sycl::free(d_src, q);
    sycl::free(d_out, q);
    pgaccel_record_gpu_exec();
    return PGACCEL_OK;
  } catch (const std::exception& e) {
    fprintf(stderr, "pgaccel: SYCL raster_hillshade failed: %s\n", e.what());
    return PGACCEL_ERROR;
  }
} catch (const pgaccel_no_device_error&) {
  return PGACCEL_ERROR_NO_DEVICE;
} catch (const std::exception& e) {
  return pgaccel_kernel_failure("pgaccel_raster_hillshade", &e);
} catch (...) {
  return pgaccel_kernel_failure("pgaccel_raster_hillshade", nullptr);
}

/* ── Raster Value (single-pixel lookup at point coords) ───────── */
/*
 * Looks up the pixel value at each (x, y) world coordinate in the input
 * point array. Translates (x, y) → (col, row) via the raster's affine,
 * bounds-checks, and writes the pixel value into output[i]. Out-of-bounds
 * points get NaN. Pixel buffer is fp32; output is fp64.
 */
extern "C" pgaccel_status pgaccel_raster_value(const float* rast_pixels, size_t width,
                                               size_t height, double origin_x, double origin_y,
                                               double scale_x, double scale_y,
                                               const double* point_xy, size_t point_count,
                                               double* output) try {
  if (rast_pixels == nullptr || point_xy == nullptr || output == nullptr) {
    return PGACCEL_ERROR_INIT;
  }
  if (width == 0 || height == 0 || point_count == 0) {
    return PGACCEL_OK;
  }
  if (scale_x == 0.0 || scale_y == 0.0) {
    return PGACCEL_ERROR_INIT;
  }

  try {
    auto& q = get_queue();
    const size_t n = width * height;

    // Translate world (x, y) -> (col, row) host-side and pass as int32
    // pairs. Doing the division inside the SYCL kernel exposed the Metal
    // soft-fp64 reciprocal bug that returns 0 for `(x - ox) / sx`
    // (cold-cache failure on M2 Max: raster_value lookup at (2.5, -1.5)
    // expected 12 got 0, 2026-05-02). The kernel now just does an array
    // index — no fp64 arithmetic on device.
    constexpr int32_t OOB_SENTINEL = INT32_MIN;
    std::vector<int32_t> col_row(point_count * 2);
    for (size_t i = 0; i < point_count; ++i) {
      const double x = point_xy[i * 2];
      const double y = point_xy[i * 2 + 1];
      const double col_d = std::floor((x - origin_x) / scale_x);
      const double row_d = std::floor((y - origin_y) / scale_y);
      if (col_d < 0.0 || row_d < 0.0 || col_d >= static_cast<double>(width) ||
          row_d >= static_cast<double>(height)) {
        col_row[i * 2] = OOB_SENTINEL;
        col_row[i * 2 + 1] = OOB_SENTINEL;
      } else {
        col_row[i * 2] = static_cast<int32_t>(col_d);
        col_row[i * 2 + 1] = static_cast<int32_t>(row_d);
      }
    }

    float* d_rast = sycl::malloc_device<float>(n, q);
    int32_t* d_idx = sycl::malloc_device<int32_t>(point_count * 2, q);
    double* d_out = sycl::malloc_device<double>(point_count, q);
    if (!d_rast || !d_idx || !d_out) {
      sycl::free(d_rast, q);
      sycl::free(d_idx, q);
      sycl::free(d_out, q);
      return PGACCEL_ERROR_OOM;
    }

    q.memcpy(d_rast, rast_pixels, n * sizeof(float));
    q.memcpy(d_idx, col_row.data(), point_count * 2 * sizeof(int32_t));
    q.wait_and_throw();

    const size_t w = width;
    // NaN bit pattern carried as uint64; sycl::bit_cast inside the kernel
    // produces the fp64 NaN without needing the host `<cmath>` `nan`
    // builtin (Metal SSCP rejects sycl::nan).
    const uint64_t nan_bits = static_cast<uint64_t>(0x7ff8000000000000ULL);

    q.submit([&](sycl::handler& h) {
       h.parallel_for(sycl::range<1>(point_count), [=](sycl::id<1> id) {
         const size_t i = id[0];
         const int32_t col = d_idx[i * 2];
         const int32_t row = d_idx[i * 2 + 1];
         if (col == OOB_SENTINEL) {
           d_out[i] = sycl::bit_cast<double>(nan_bits);
           return;
         }
         d_out[i] =
             static_cast<double>(d_rast[static_cast<size_t>(row) * w + static_cast<size_t>(col)]);
       });
     }).wait_and_throw();
    sycl::free(d_idx, q);

    q.memcpy(output, d_out, point_count * sizeof(double)).wait_and_throw();

    sycl::free(d_rast, q);
    sycl::free(d_out, q);
    pgaccel_record_gpu_exec();
    return PGACCEL_OK;
  } catch (const std::exception& e) {
    fprintf(stderr, "pgaccel: SYCL raster_value failed: %s\n", e.what());
    return PGACCEL_ERROR;
  }
} catch (const pgaccel_no_device_error&) {
  return PGACCEL_ERROR_NO_DEVICE;
} catch (const std::exception& e) {
  return pgaccel_kernel_failure("pgaccel_raster_value", &e);
} catch (...) {
  return pgaccel_kernel_failure("pgaccel_raster_value", nullptr);
}

/* ── Raster SummaryStats (count/sum/mean/stddev/min/max per row) ─ */
/*
 * Per raster-row, computes 6 summary statistics over its pixels:
 *   output[r*6 + 0] = count   (non-NaN, non-NoData pixel count, fp64)
 *   output[r*6 + 1] = sum
 *   output[r*6 + 2] = mean
 *   output[r*6 + 3] = stddev  (population, sqrt(E[X^2] - E[X]^2))
 *   output[r*6 + 4] = min
 *   output[r*6 + 5] = max
 *
 * `pixels_per_row` is constant across rows (one raster geometry per
 * input row, all rasters same dimensions). When `nodata_masks` is
 * non-null, the mask pixel-by-pixel — `1` = NoData, skipped from stats.
 *
 * Output buffer = 6 * sizeof(double) * row_count. Coordinates with
 * `OutputShape::Record { field_count: 6 }` on the Rust side.
 */
extern "C" pgaccel_status pgaccel_raster_summarystats(const float* rast_pixels, size_t row_count,
                                                      size_t pixels_per_row,
                                                      const uint8_t* nodata_masks, double* output) try {
  if (rast_pixels == nullptr || output == nullptr) {
    return PGACCEL_ERROR_INIT;
  }
  if (row_count == 0 || pixels_per_row == 0) {
    return PGACCEL_OK;
  }

  try {
    auto& q = get_queue();
    const size_t n = row_count * pixels_per_row;

    float* d_pix = sycl::malloc_device<float>(n, q);
    uint8_t* d_mask = nodata_masks ? sycl::malloc_device<uint8_t>(n, q) : nullptr;
    double* d_out = sycl::malloc_device<double>(row_count * 6, q);
    if (!d_pix || !d_out || (nodata_masks && !d_mask)) {
      sycl::free(d_pix, q);
      sycl::free(d_mask, q);
      sycl::free(d_out, q);
      return PGACCEL_ERROR_OOM;
    }

    q.memcpy(d_pix, rast_pixels, n * sizeof(float));
    if (nodata_masks) {
      q.memcpy(d_mask, nodata_masks, n);
    }
    q.wait_and_throw();

    const size_t ppr = pixels_per_row;
    const bool has_mask = (nodata_masks != nullptr);
    uint8_t* d_mask_capture = d_mask;

    q.submit([&](sycl::handler& h) {
       h.parallel_for(sycl::range<1>(row_count), [=](sycl::id<1> id) {
         const size_t r = id[0];
         const size_t base = r * ppr;
         double cnt = 0.0;
         double sum = 0.0;
         double sum_sq = 0.0;
         double mn = 0.0;
         double mx = 0.0;
         bool have_any = false;

         for (size_t i = 0; i < ppr; ++i) {
           if (has_mask && d_mask_capture[base + i] != 0)
             continue;
           const double v = static_cast<double>(d_pix[base + i]);
           if (sycl::isnan(v) || sycl::isinf(v))
             continue;
           if (!have_any) {
             mn = v;
             mx = v;
             have_any = true;
           } else {
             if (v < mn)
               mn = v;
             if (v > mx)
               mx = v;
           }
           cnt += 1.0;
           sum += v;
           sum_sq += v * v;
         }

         d_out[r * 6 + 0] = cnt;
         d_out[r * 6 + 1] = sum;
         // Slot 2 holds sum_sq until host derives mean from sum / cnt.
         // Slot 3 stays 0 until host derives stddev. Computing mean/stddev
         // device-side returned 0 under Metal soft-fp64 (sum / cnt → 0 in
         // cold-cache testing on M2 Max, test_raster row 0 mean / masked
         // mean failures, 2026-05-02). Move the ratio host-side to keep
         // every code path SYCL-only without tripping soft-fp64 reciprocal.
         d_out[r * 6 + 2] = sum_sq;
         d_out[r * 6 + 3] = 0.0;
         d_out[r * 6 + 4] = have_any ? mn : 0.0;
         d_out[r * 6 + 5] = have_any ? mx : 0.0;
       });
     }).wait_and_throw();

    q.memcpy(output, d_out, row_count * 6 * sizeof(double)).wait_and_throw();

    // Host-side: convert (sum_sq, 0) placeholders into (mean, stddev).
    for (size_t r = 0; r < row_count; ++r) {
      const double cnt = output[r * 6 + 0];
      const double sum = output[r * 6 + 1];
      const double sum_sq = output[r * 6 + 2];
      const double mean = (cnt > 0.0) ? sum / cnt : 0.0;
      double variance = (cnt > 0.0) ? (sum_sq / cnt - mean * mean) : 0.0;
      if (variance < 0.0) {
        variance = 0.0;  // floating-point noise guard
      }
      output[r * 6 + 2] = mean;
      output[r * 6 + 3] = std::sqrt(variance);
    }

    sycl::free(d_pix, q);
    sycl::free(d_mask, q);
    sycl::free(d_out, q);
    pgaccel_record_gpu_exec();
    return PGACCEL_OK;
  } catch (const std::exception& e) {
    fprintf(stderr, "pgaccel: SYCL raster_summarystats failed: %s\n", e.what());
    return PGACCEL_ERROR;
  }
} catch (const pgaccel_no_device_error&) {
  return PGACCEL_ERROR_NO_DEVICE;
} catch (const std::exception& e) {
  return pgaccel_kernel_failure("pgaccel_raster_summarystats", &e);
} catch (...) {
  return pgaccel_kernel_failure("pgaccel_raster_summarystats", nullptr);
}

extern "C" pgaccel_status pgaccel_raster_reclass(const void* input_pixels, size_t pixel_count,
                                                 int input_type, const pgaccel_reclass_rule* rules,
                                                 size_t rule_count, int output_type,
                                                 void* output_pixels) try {
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
    if (in_psz == 0 || out_psz == 0)
      return PGACCEL_ERROR_UNSUPPORTED;

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
      sycl::free(d_in, q);
      sycl::free(d_out, q);
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
         float out_val = val;  // passthrough by default

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
      sycl::free(d_in, q);
      sycl::free(d_out, q);
      sycl::free(d_rules, q);
      return PGACCEL_ERROR_OOM;
    }

    q.memcpy(h_out, d_out, pixel_count * sizeof(float)).wait_and_throw();

    for (size_t i = 0; i < pixel_count; i++) {
      write_pixel(output_pixels, i, output_type, static_cast<double>(h_out[i]));
    }

    delete[] h_out;
    sycl::free(d_in, q);
    sycl::free(d_out, q);
    sycl::free(d_rules, q);
    pgaccel_record_gpu_exec();
    return PGACCEL_OK;
  } catch (const pgaccel_no_device_error&) {
    return PGACCEL_ERROR_NO_DEVICE;
  } catch (const std::exception& e) {
    return pgaccel_kernel_failure(__func__, &e);
  } catch (...) {
    return pgaccel_kernel_failure(__func__, nullptr);
  }
  return PGACCEL_ERROR_NO_DEVICE;
} catch (const pgaccel_no_device_error&) {
  return PGACCEL_ERROR_NO_DEVICE;
} catch (const std::exception& e) {
  return pgaccel_kernel_failure("pgaccel_raster_reclass", &e);
} catch (...) {
  return pgaccel_kernel_failure("pgaccel_raster_reclass", nullptr);
}

/* ── Exact resident PostGIS Reclass ───────────────────────────── */

namespace {

struct RasterResidentSpan {
  uintptr_t begin;
  uintptr_t end;
  bool active;
};

bool raster_resident_exact_span(const void* pointer, size_t bytes, RasterResidentSpan* span) {
  if (bytes == 0) {
    *span = {0, 0, false};
    return pointer == nullptr;
  }
  if (pointer == nullptr)
    return false;
  const uintptr_t begin = reinterpret_cast<uintptr_t>(pointer);
  if (begin > std::numeric_limits<uintptr_t>::max() - bytes)
    return false;
  *span = {begin, begin + bytes, true};
  return true;
}

bool raster_resident_spans_overlap(const RasterResidentSpan& lhs, const RasterResidentSpan& rhs) {
  return lhs.active && rhs.active && lhs.begin < rhs.end && rhs.begin < lhs.end;
}

bool raster_resident_checked_bytes(size_t count, size_t width, size_t* bytes) {
  if (width == 0 || count > std::numeric_limits<size_t>::max() / width)
    return false;
  *bytes = count * width;
  return true;
}

bool raster_resident_launch_count_within_limit(size_t total, size_t chunk) {
  if (chunk == 0)
    return false;
  const size_t launches = total == 0 ? 0 : 1 + (total - 1) / chunk;
  return launches <= PGACCEL_RESIDENT_RASTER_MAX_LAUNCH_CHUNKS;
}

bool raster_resident_current_device_pointer(sycl::queue& queue, const void* pointer) {
  try {
    const sycl::usm::alloc allocation = sycl::get_pointer_type(pointer, queue.get_context());
    return (allocation == sycl::usm::alloc::device || allocation == sycl::usm::alloc::shared) &&
           sycl::get_pointer_device(pointer, queue.get_context()) == queue.get_device();
  } catch (...) {
    return false;
  }
}

size_t raster_resident_pixel_width(uint32_t pixel_type) {
  switch (pixel_type) {
    case PGACCEL_RESIDENT_RASTER_BOOL:
    case PGACCEL_RESIDENT_RASTER_UINT2:
    case PGACCEL_RESIDENT_RASTER_UINT4:
    case PGACCEL_RESIDENT_RASTER_INT8:
    case PGACCEL_RESIDENT_RASTER_UINT8:
      return 1;
    case PGACCEL_RESIDENT_RASTER_INT16:
    case PGACCEL_RESIDENT_RASTER_UINT16:
      return 2;
    case PGACCEL_RESIDENT_RASTER_INT32:
    case PGACCEL_RESIDENT_RASTER_UINT32:
    case PGACCEL_RESIDENT_RASTER_FLOAT32:
      return 4;
    case PGACCEL_RESIDENT_RASTER_FLOAT64:
      return 8;
    default:
      return 0;
  }
}

inline uint32_t raster_resident_width_shift(size_t width) {
  return width == 1 ? 0 : (width == 2 ? 1 : (width == 4 ? 2 : 3));
}

bool raster_resident_integer_bounds(uint32_t pixel_type, int64_t* minimum, int64_t* maximum) {
  switch (pixel_type) {
    case PGACCEL_RESIDENT_RASTER_BOOL:
      *minimum = 0;
      *maximum = 1;
      return true;
    case PGACCEL_RESIDENT_RASTER_UINT2:
      *minimum = 0;
      *maximum = 3;
      return true;
    case PGACCEL_RESIDENT_RASTER_UINT4:
      *minimum = 0;
      *maximum = 15;
      return true;
    case PGACCEL_RESIDENT_RASTER_INT8:
      *minimum = INT8_MIN;
      *maximum = INT8_MAX;
      return true;
    case PGACCEL_RESIDENT_RASTER_UINT8:
      *minimum = 0;
      *maximum = UINT8_MAX;
      return true;
    case PGACCEL_RESIDENT_RASTER_INT16:
      *minimum = INT16_MIN;
      *maximum = INT16_MAX;
      return true;
    case PGACCEL_RESIDENT_RASTER_UINT16:
      *minimum = 0;
      *maximum = UINT16_MAX;
      return true;
    case PGACCEL_RESIDENT_RASTER_INT32:
      *minimum = INT32_MIN;
      *maximum = INT32_MAX;
      return true;
    case PGACCEL_RESIDENT_RASTER_UINT32:
      *minimum = 0;
      *maximum = static_cast<int64_t>(UINT32_MAX);
      return true;
    default:
      return false;
  }
}

inline uint16_t raster_resident_load_u16_le(const uint8_t* pointer) {
  return static_cast<uint16_t>(pointer[0]) |
         static_cast<uint16_t>(static_cast<uint16_t>(pointer[1]) << 8);
}

inline uint32_t raster_resident_load_u32_le(const uint8_t* pointer) {
  return static_cast<uint32_t>(pointer[0]) | (static_cast<uint32_t>(pointer[1]) << 8) |
         (static_cast<uint32_t>(pointer[2]) << 16) | (static_cast<uint32_t>(pointer[3]) << 24);
}

inline uint64_t raster_resident_load_u64_le(const uint8_t* pointer) {
  uint64_t value = 0;
  for (uint32_t byte = 0; byte < 8; ++byte)
    value |= static_cast<uint64_t>(pointer[byte]) << (byte * 8);
  return value;
}

inline double raster_resident_read_pixel(const uint8_t* pointer, uint32_t pixel_type) {
  switch (pixel_type) {
    case PGACCEL_RESIDENT_RASTER_BOOL:
    case PGACCEL_RESIDENT_RASTER_UINT2:
    case PGACCEL_RESIDENT_RASTER_UINT4:
    case PGACCEL_RESIDENT_RASTER_UINT8:
      return static_cast<double>(pointer[0]);
    case PGACCEL_RESIDENT_RASTER_INT8: {
      const int32_t value = pointer[0] < 0x80 ? static_cast<int32_t>(pointer[0])
                                              : static_cast<int32_t>(pointer[0]) - 0x100;
      return static_cast<double>(value);
    }
    case PGACCEL_RESIDENT_RASTER_INT16: {
      const uint16_t raw = raster_resident_load_u16_le(pointer);
      const int32_t value =
          raw < 0x8000 ? static_cast<int32_t>(raw) : static_cast<int32_t>(raw) - 0x10000;
      return static_cast<double>(value);
    }
    case PGACCEL_RESIDENT_RASTER_UINT16:
      return static_cast<double>(raster_resident_load_u16_le(pointer));
    case PGACCEL_RESIDENT_RASTER_INT32: {
      const uint32_t raw = raster_resident_load_u32_le(pointer);
      const int64_t value =
          raw < 0x80000000u ? static_cast<int64_t>(raw) : static_cast<int64_t>(raw) - 0x100000000ll;
      return static_cast<double>(value);
    }
    case PGACCEL_RESIDENT_RASTER_UINT32:
      return static_cast<double>(raster_resident_load_u32_le(pointer));
    case PGACCEL_RESIDENT_RASTER_FLOAT32:
      return static_cast<double>(sycl::bit_cast<float>(raster_resident_load_u32_le(pointer)));
    case PGACCEL_RESIDENT_RASTER_FLOAT64:
      return sycl::bit_cast<double>(raster_resident_load_u64_le(pointer));
    default:
      return 0.0;
  }
}

inline void raster_resident_write_integer(uint8_t* pointer, uint32_t pixel_type, int64_t value) {
  const uint64_t raw = static_cast<uint64_t>(value);
  const size_t width = raster_resident_pixel_width(pixel_type);
  for (size_t byte = 0; byte < width; ++byte)
    pointer[byte] = static_cast<uint8_t>((raw >> (byte * 8)) & 0xffu);
}

inline bool raster_resident_positive_zero(double value) {
  return sycl::bit_cast<uint64_t>(value) == 0;
}

inline bool raster_resident_row_is_canonical_null(const pgaccel_resident_raster_row& row) {
  return row.width == 0 && row.height == 0 && row.first_band == 0 && row.band_count == 0 &&
         row.srid == 0 && row.flags == 0 && raster_resident_positive_zero(row.scale_x) &&
         raster_resident_positive_zero(row.scale_y) && raster_resident_positive_zero(row.ip_x) &&
         raster_resident_positive_zero(row.ip_y) && raster_resident_positive_zero(row.skew_x) &&
         raster_resident_positive_zero(row.skew_y);
}

constexpr uint32_t RASTER_RESIDENT_FAILURE_VIEW = 1u << 0;
constexpr uint32_t RASTER_RESIDENT_FAILURE_RULES = 1u << 1;
constexpr uint32_t RASTER_RESIDENT_FAILURE_OFFSETS = 1u << 2;
constexpr uint32_t RASTER_RESIDENT_FAILURE_CAPACITY = 1u << 3;
constexpr uint32_t RASTER_RESIDENT_FAILURE_BYTE_BUDGET = 1u << 4;
constexpr uint32_t RASTER_RESIDENT_FAILURE_NUMERIC = 1u << 5;
static_assert(RASTER_RESIDENT_FAILURE_VIEW == PGACCEL_RASTER_VALIDATION_VIEW);
static_assert(RASTER_RESIDENT_FAILURE_RULES == PGACCEL_RASTER_VALIDATION_RULES);
static_assert(RASTER_RESIDENT_FAILURE_OFFSETS == PGACCEL_RASTER_VALIDATION_OFFSETS);
static_assert(RASTER_RESIDENT_FAILURE_CAPACITY == PGACCEL_RASTER_VALIDATION_CAPACITY);
static_assert(RASTER_RESIDENT_FAILURE_BYTE_BUDGET == PGACCEL_RASTER_VALIDATION_BYTE_BUDGET);
static_assert(RASTER_RESIDENT_FAILURE_NUMERIC == PGACCEL_RASTER_VALIDATION_NUMERIC_OVERFLOW);
static_assert(sizeof(size_t) == sizeof(uint64_t), "resident raster ABI requires LP64 size_t");
constexpr size_t RASTER_RESIDENT_MAX_ROW_VALIDATION_LAUNCH =
    PGACCEL_RESIDENT_RASTER_ROWS_PER_VALIDATION_LAUNCH;

class RasterResidentRuleValidationKernel;
class RasterResidentRowValidationKernel;
class RasterResidentLowBitValidationKernel;
class RasterResidentRowActionKernel;
class RasterResidentReclassKernel;

}  // namespace

extern "C" pgaccel_status
pgaccel_raster_reclass_resident_ex(const pgaccel_raster_reclass_resident_request* request,
                                   int32_t* detail) try {
  if (detail == nullptr)
    return PGACCEL_INVALID_ARGUMENT;
  *detail = PGACCEL_RASTER_DETAIL_NONE;
  if (request == nullptr) {
    *detail = PGACCEL_RASTER_DETAIL_CONTRACT;
    return PGACCEL_INVALID_ARGUMENT;
  }
  const pgaccel_resident_raster_view& view = request->input;
  if (request->abi_version != PGACCEL_RESIDENT_RASTER_ABI_VERSION || request->flags != 0 ||
      request->pad != 0 || view.abi_version != PGACCEL_RESIDENT_RASTER_ABI_VERSION ||
      view.flags != 0 || request->first_row > view.row_count ||
      request->count > view.row_count - request->first_row) {
    *detail = PGACCEL_RASTER_DETAIL_CONTRACT;
    return PGACCEL_INVALID_ARGUMENT;
  }
  if (request->count == 0)
    return PGACCEL_OK;

  int64_t output_minimum = 0;
  int64_t output_maximum = 0;
  const size_t output_width = raster_resident_pixel_width(request->output_pixel_type);
  if (!raster_resident_integer_bounds(request->output_pixel_type, &output_minimum,
                                      &output_maximum) ||
      output_width == 0) {
    *detail = PGACCEL_RASTER_DETAIL_CONTRACT;
    return PGACCEL_INVALID_ARGUMENT;
  }

  size_t expected_rows_bytes = 0;
  size_t expected_bands_bytes = 0;
  size_t band_offset_count = 0;
  size_t expected_band_offsets_bytes = 0;
  size_t expected_rules_bytes = 0;
  size_t output_offset_count = 0;
  size_t expected_output_offsets_bytes = 0;
  size_t expected_total_output_bytes = 0;
  if (!raster_resident_checked_bytes(view.row_count, sizeof(pgaccel_resident_raster_row),
                                     &expected_rows_bytes) ||
      !raster_resident_checked_bytes(view.band_count, sizeof(pgaccel_resident_raster_band),
                                     &expected_bands_bytes) ||
      view.band_count == std::numeric_limits<size_t>::max() ||
      (band_offset_count = view.band_count + 1,
       !raster_resident_checked_bytes(band_offset_count, sizeof(uint64_t),
                                      &expected_band_offsets_bytes)) ||
      !raster_resident_checked_bytes(request->rule_count,
                                     sizeof(pgaccel_resident_raster_reclass_rule),
                                     &expected_rules_bytes) ||
      request->count == std::numeric_limits<size_t>::max() ||
      (output_offset_count = request->count + 1,
       !raster_resident_checked_bytes(output_offset_count, sizeof(uint64_t),
                                      &expected_output_offsets_bytes)) ||
      !raster_resident_checked_bytes(request->max_total_pixels, output_width,
                                     &expected_total_output_bytes)) {
    *detail = PGACCEL_RASTER_DETAIL_NUMERIC_OVERFLOW;
    return PGACCEL_INVALID_ARGUMENT;
  }

  if (view.rows_bytes != expected_rows_bytes || view.bands_bytes != expected_bands_bytes ||
      view.band_offsets_bytes != expected_band_offsets_bytes ||
      (view.nulls_bytes != 0 && view.nulls_bytes != view.row_count) || request->rule_count == 0 ||
      request->rule_count > PGACCEL_RESIDENT_RASTER_MAX_RECLASS_RULES ||
      request->rules_bytes != expected_rules_bytes ||
      request->output_offsets_bytes != expected_output_offsets_bytes ||
      request->row_actions_bytes != request->count ||
      request->validation_scratch_bytes != sizeof(pgaccel_resident_raster_validation_scratch) ||
      request->max_chunk_pixels == 0 ||
      (request->max_total_pixels != 0 && request->max_chunk_pixels > request->max_total_pixels)) {
    *detail = PGACCEL_RASTER_DETAIL_CONTRACT;
    return PGACCEL_INVALID_ARGUMENT;
  }
  if (expected_total_output_bytes > request->output_pixels_bytes) {
    *detail = PGACCEL_RASTER_DETAIL_CAPACITY;
    return PGACCEL_INVALID_ARGUMENT;
  }
  if (!raster_resident_launch_count_within_limit(request->max_total_pixels,
                                                 request->max_chunk_pixels) ||
      !raster_resident_launch_count_within_limit(request->count,
                                                 RASTER_RESIDENT_MAX_ROW_VALIDATION_LAUNCH)) {
    *detail = PGACCEL_RASTER_DETAIL_BYTE_BUDGET;
    return PGACCEL_INVALID_ARGUMENT;
  }

  auto aligned_pointer = [](const void* pointer, size_t alignment) {
    return pointer == nullptr || reinterpret_cast<uintptr_t>(pointer) % alignment == 0;
  };
  if (!aligned_pointer(view.band_offsets, alignof(uint64_t)) ||
      !aligned_pointer(view.rows, alignof(pgaccel_resident_raster_row)) ||
      !aligned_pointer(view.bands, alignof(pgaccel_resident_raster_band)) ||
      !aligned_pointer(request->rules, alignof(pgaccel_resident_raster_reclass_rule)) ||
      !aligned_pointer(request->output_offsets, alignof(uint64_t)) ||
      !aligned_pointer(request->validation_scratch,
                       alignof(pgaccel_resident_raster_validation_scratch))) {
    *detail = PGACCEL_RASTER_DETAIL_CONTRACT;
    return PGACCEL_INVALID_ARGUMENT;
  }

  RasterResidentSpan spans[10]{};
  size_t span_count = 0;
  auto add_span = [&](const void* pointer, size_t bytes) {
    RasterResidentSpan span{};
    if (!raster_resident_exact_span(pointer, bytes, &span))
      return false;
    if (span.active)
      spans[span_count++] = span;
    return true;
  };
  if (!add_span(view.pixels, view.pixels_bytes) ||
      !add_span(view.band_offsets, view.band_offsets_bytes) ||
      !add_span(view.rows, view.rows_bytes) || !add_span(view.bands, view.bands_bytes) ||
      !add_span(view.nulls, view.nulls_bytes) || !add_span(request->rules, request->rules_bytes) ||
      !add_span(request->output_offsets, request->output_offsets_bytes) ||
      !add_span(request->output_pixels, request->output_pixels_bytes) ||
      !add_span(request->row_actions, request->row_actions_bytes) ||
      !add_span(request->validation_scratch, request->validation_scratch_bytes)) {
    *detail = PGACCEL_RASTER_DETAIL_CONTRACT;
    return PGACCEL_INVALID_ARGUMENT;
  }
  for (size_t left = 0; left < span_count; ++left) {
    for (size_t right = left + 1; right < span_count; ++right) {
      if (raster_resident_spans_overlap(spans[left], spans[right])) {
        *detail = PGACCEL_RASTER_DETAIL_CONTRACT;
        return PGACCEL_INVALID_ARGUMENT;
      }
    }
  }

  sycl::queue& queue = get_queue();
  for (size_t span = 0; span < span_count; ++span) {
    if (!raster_resident_current_device_pointer(queue,
                                                reinterpret_cast<const void*>(spans[span].begin))) {
      *detail = PGACCEL_RASTER_DETAIL_CONTRACT;
      return PGACCEL_INVALID_ARGUMENT;
    }
  }

  auto* validation = request->validation_scratch;
  queue.memset(validation, 0, sizeof(*validation));

  const pgaccel_resident_raster_view input = view;
  const auto* rules = request->rules;
  const auto* output_offsets = request->output_offsets;
  auto* output_pixels = request->output_pixels;
  auto* row_actions = request->row_actions;
  const size_t rule_count = request->rule_count;
  const int64_t minimum = output_minimum;
  const int64_t maximum = output_maximum;
  queue.parallel_for<RasterResidentRuleValidationKernel>(
      sycl::range<1>(rule_count), [=](sycl::id<1> id) {
        const size_t index = id[0];
        const pgaccel_resident_raster_reclass_rule rule = rules[index];
        const bool invalid = rule.source < static_cast<int64_t>(INT32_MIN) ||
                             rule.source > static_cast<int64_t>(UINT32_MAX) ||
                             rule.destination < minimum || rule.destination > maximum ||
                             (index > 0 && rules[index - 1].source >= rule.source);
        if (invalid) {
          sycl::atomic_ref<uint32_t, sycl::memory_order::relaxed, sycl::memory_scope::device,
                           sycl::access::address_space::global_space>
              failures(validation->failures);
          failures.fetch_or(RASTER_RESIDENT_FAILURE_RULES);
        }
      });

  const size_t selected_first_row = request->first_row;
  const size_t selected_count = request->count;
  const size_t output_capacity = request->output_pixels_bytes;
  const uint32_t output_type = request->output_pixel_type;
  const size_t output_element_bytes = output_width;
  const uint32_t output_element_shift = raster_resident_width_shift(output_width);
  const size_t exact_total_output_bytes = expected_total_output_bytes;
  for (size_t launch_start = 0; launch_start < selected_count;) {
    const size_t launch_count =
        std::min(RASTER_RESIDENT_MAX_ROW_VALIDATION_LAUNCH, selected_count - launch_start);
    queue.parallel_for<RasterResidentRowValidationKernel>(
        sycl::range<1>(launch_count), [=](sycl::id<1> id) {
          const size_t local_row = launch_start + id[0];
          const size_t row_index = selected_first_row + local_row;
          const pgaccel_resident_raster_row row = input.rows[row_index];
          const uint8_t null_byte = input.nulls == nullptr ? 0 : input.nulls[row_index];
          const uint64_t output_start = output_offsets[local_row];
          const uint64_t output_end = output_offsets[local_row + 1];
          uint32_t row_failure = 0;
          if (local_row == 0)
            validation->first_output_offset = output_start;
          if (local_row + 1 == selected_count) {
            validation->last_output_offset = output_end;
            const uint64_t first_output = output_offsets[0];
            if (output_end < first_output || output_end - first_output != exact_total_output_bytes)
              row_failure |= RASTER_RESIDENT_FAILURE_BYTE_BUDGET;
          }

          if (output_start > output_end || output_start % output_element_bytes != 0 ||
              output_end % output_element_bytes != 0)
            row_failure |= RASTER_RESIDENT_FAILURE_OFFSETS;
          if (output_start > output_capacity || output_end > output_capacity)
            row_failure |= RASTER_RESIDENT_FAILURE_CAPACITY;
          if (local_row == 0 && (input.band_offsets[0] != 0 ||
                                 input.band_offsets[input.band_count] != input.pixels_bytes))
            row_failure |= RASTER_RESIDENT_FAILURE_VIEW;

          uint64_t expected_output_bytes = 0;
          if (null_byte > 1) {
            row_failure |= RASTER_RESIDENT_FAILURE_VIEW;
          } else if (null_byte != 0) {
            if (!raster_resident_row_is_canonical_null(row))
              row_failure |= RASTER_RESIDENT_FAILURE_VIEW;
          } else {
            const bool finite_metadata = sycl::isfinite(row.scale_x) &&
                                         sycl::isfinite(row.scale_y) && sycl::isfinite(row.ip_x) &&
                                         sycl::isfinite(row.ip_y) && sycl::isfinite(row.skew_x) &&
                                         sycl::isfinite(row.skew_y);
            const uint64_t first_band = row.first_band;
            const uint64_t band_end = first_band + row.band_count;
            if (row.flags != 0 || row.srid < 0 || row.srid > 999999 || !finite_metadata ||
                first_band > input.band_count || band_end > input.band_count) {
              row_failure |= RASTER_RESIDENT_FAILURE_VIEW;
            } else if (row.band_count != 0) {
              const pgaccel_resident_raster_band band = input.bands[first_band];
              const size_t input_element_bytes = raster_resident_pixel_width(band.pixel_type);
              const uint64_t input_start = input.band_offsets[first_band];
              const uint64_t input_end = input.band_offsets[first_band + 1];
              const uint64_t pixel_count = static_cast<uint64_t>(row.width) * row.height;
              const uint32_t known_band_flags =
                  PGACCEL_RESIDENT_RASTER_BAND_HAS_NODATA | PGACCEL_RESIDENT_RASTER_BAND_IS_NODATA;
              const uint64_t max_bytes = std::numeric_limits<uint64_t>::max();
              const uint32_t input_element_shift =
                  raster_resident_width_shift(input_element_bytes == 0 ? 1 : input_element_bytes);
              const bool numeric_overflow =
                  input_element_bytes != 0 && (pixel_count > (max_bytes >> input_element_shift) ||
                                               pixel_count > (max_bytes >> output_element_shift));
              if (numeric_overflow) {
                row_failure |= RASTER_RESIDENT_FAILURE_NUMERIC;
              } else if (input_element_bytes == 0 || (band.flags & ~known_band_flags) != 0 ||
                         ((band.flags & PGACCEL_RESIDENT_RASTER_BAND_IS_NODATA) != 0 &&
                          (band.flags & PGACCEL_RESIDENT_RASTER_BAND_HAS_NODATA) == 0) ||
                         ((band.flags & PGACCEL_RESIDENT_RASTER_BAND_HAS_NODATA) == 0 &&
                          !raster_resident_positive_zero(band.nodata)) ||
                         input_start > input_end || input_end > input.pixels_bytes) {
                row_failure |= RASTER_RESIDENT_FAILURE_VIEW;
              } else {
                const uint64_t expected_input_bytes = pixel_count << input_element_shift;
                expected_output_bytes = pixel_count << output_element_shift;
                if (input_end - input_start != expected_input_bytes)
                  row_failure |= RASTER_RESIDENT_FAILURE_VIEW;
              }
            }
          }
          if (output_start <= output_end && output_end - output_start != expected_output_bytes)
            row_failure |= RASTER_RESIDENT_FAILURE_OFFSETS;
          if (row_failure != 0) {
            sycl::atomic_ref<uint32_t, sycl::memory_order::relaxed, sycl::memory_scope::device,
                             sycl::access::address_space::global_space>
                failures(validation->failures);
            failures.fetch_or(row_failure);
          }
        });
    launch_start += launch_count;
  }
  const size_t total_pixels = request->max_total_pixels;
  const size_t max_chunk_pixels = request->max_chunk_pixels;
  for (size_t launch_start = 0; launch_start < total_pixels;) {
    const size_t launch_count = std::min(max_chunk_pixels, total_pixels - launch_start);
    queue.parallel_for<RasterResidentLowBitValidationKernel>(
        sycl::range<1>(launch_count), [=](sycl::id<1> id) {
          if (validation->failures != 0)
            return;
          const uint64_t pixel_index = launch_start + id[0];
          const uint64_t output_byte = output_offsets[0] + (pixel_index << output_element_shift);

          size_t low = 0;
          size_t high = selected_count + 1;
          while (low < high) {
            const size_t middle = low + (high - low) / 2;
            if (output_offsets[middle] <= output_byte)
              low = middle + 1;
            else
              high = middle;
          }
          const size_t local_row = low - 1;
          const pgaccel_resident_raster_row row = input.rows[selected_first_row + local_row];
          const pgaccel_resident_raster_band band = input.bands[row.first_band];
          uint8_t allowed_bits = 0xff;
          if (band.pixel_type == PGACCEL_RESIDENT_RASTER_BOOL)
            allowed_bits = 0x01;
          else if (band.pixel_type == PGACCEL_RESIDENT_RASTER_UINT2)
            allowed_bits = 0x03;
          else if (band.pixel_type == PGACCEL_RESIDENT_RASTER_UINT4)
            allowed_bits = 0x0f;
          if (allowed_bits != 0xff) {
            const uint64_t row_pixel =
                (output_byte - output_offsets[local_row]) / output_element_bytes;
            const uint64_t input_byte = input.band_offsets[row.first_band] + row_pixel;
            if ((input.pixels[input_byte] & ~allowed_bits) != 0) {
              sycl::atomic_ref<uint32_t, sycl::memory_order::relaxed, sycl::memory_scope::device,
                               sycl::access::address_space::global_space>
                  failures(validation->failures);
              failures.fetch_or(RASTER_RESIDENT_FAILURE_VIEW);
            }
          }
        });
    launch_start += launch_count;
  }

  for (size_t launch_start = 0; launch_start < selected_count;) {
    const size_t launch_count =
        std::min(RASTER_RESIDENT_MAX_ROW_VALIDATION_LAUNCH, selected_count - launch_start);
    queue.parallel_for<RasterResidentRowActionKernel>(
        sycl::range<1>(launch_count), [=](sycl::id<1> id) {
          if (validation->failures != 0)
            return;
          const size_t local_row = launch_start + id[0];
          const size_t row_index = selected_first_row + local_row;
          if (input.nulls != nullptr && input.nulls[row_index] != 0)
            row_actions[local_row] = PGACCEL_RASTER_ROW_NULL;
          else if (input.rows[row_index].band_count == 0)
            row_actions[local_row] = PGACCEL_RASTER_ROW_PASSTHROUGH;
          else
            row_actions[local_row] = PGACCEL_RASTER_ROW_RECLASSIFIED;
        });
    launch_start += launch_count;
  }

  for (size_t launch_start = 0; launch_start < total_pixels;) {
    const size_t launch_count = std::min(max_chunk_pixels, total_pixels - launch_start);
    queue.parallel_for<RasterResidentReclassKernel>(
        sycl::range<1>(launch_count), [=](sycl::id<1> id) {
          if (validation->failures != 0)
            return;
          const uint64_t pixel_index = launch_start + id[0];
          const uint64_t output_byte = output_offsets[0] + (pixel_index << output_element_shift);

          size_t low = 0;
          size_t high = selected_count + 1;
          while (low < high) {
            const size_t middle = low + (high - low) / 2;
            if (output_offsets[middle] <= output_byte)
              low = middle + 1;
            else
              high = middle;
          }
          const size_t local_row = low - 1;
          const pgaccel_resident_raster_row row = input.rows[selected_first_row + local_row];
          const pgaccel_resident_raster_band band = input.bands[row.first_band];
          const size_t input_element_bytes = raster_resident_pixel_width(band.pixel_type);
          const uint32_t input_element_shift = raster_resident_width_shift(input_element_bytes);
          const uint64_t row_pixel =
              (output_byte - output_offsets[local_row]) / output_element_bytes;
          const uint64_t input_byte =
              input.band_offsets[row.first_band] + (row_pixel << input_element_shift);
          const double value =
              (band.flags & PGACCEL_RESIDENT_RASTER_BAND_IS_NODATA) != 0
                  ? band.nodata
                  : raster_resident_read_pixel(input.pixels + input_byte, band.pixel_type);

          int64_t destination = 0;
          constexpr double kPostgisFltEpsilon = 1.1920928955078125e-7;
          for (size_t rule_index = 0; rule_index < rule_count; ++rule_index) {
            const double source = static_cast<double>(rules[rule_index].source);
            if (source == value || sycl::fabs(source - value) <= kPostgisFltEpsilon) {
              destination = rules[rule_index].destination;
              break;
            }
          }
          raster_resident_write_integer(output_pixels + output_byte, output_type, destination);
        });
    launch_start += launch_count;
  }
  queue.wait_and_throw();
  pgaccel_record_gpu_exec();
  return PGACCEL_OK;
} catch (const pgaccel_no_device_error&) {
  return PGACCEL_ERROR_NO_DEVICE;
} catch (const std::bad_alloc&) {
  return PGACCEL_OOM;
} catch (const std::exception& error) {
  return pgaccel_kernel_failure("pgaccel_raster_reclass_resident_ex", &error);
} catch (...) {
  return pgaccel_kernel_failure("pgaccel_raster_reclass_resident_ex", nullptr);
}

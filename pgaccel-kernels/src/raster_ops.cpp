#include <sycl/sycl.hpp>

#include <cmath>
#include <cstdint>
#include <cstdio>
#include <cstring>
#include <new>
#include <stdexcept>
#include <vector>

#include "pgaccel_ffi.h"

// SAFETY: g_queue is owned by device_manager.cpp. Raster kernels must share
// the same process-global queue so pgaccel_shutdown() can release the Metal
// context; a private static queue leaks an extra runtime context at exit.
extern sycl::queue* g_queue;

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
  if (g_queue == nullptr && pgaccel_init() != PGACCEL_OK) {
    throw std::runtime_error("pgaccel_init failed");
  }
  if (g_queue == nullptr) {
    throw std::runtime_error("pgaccel queue unavailable");
  }
  return *g_queue;
}

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

      // Allocate per-band device buffers.
      std::vector<float*> d_bands(band_count);
      for (size_t b = 0; b < band_count; ++b) {
        d_bands[b] = sycl::malloc_device<float>(pixel_count, q);
        if (!d_bands[b]) {
          for (size_t j = 0; j < b; ++j)
            sycl::free(d_bands[j], q);
          sycl::free(d_inst, q);
          return PGACCEL_ERROR_OOM;
        }
        q.memcpy(d_bands[b], band_pixels[b], pixel_count * sizeof(float));
      }
      float* d_out = sycl::malloc_device<float>(pixel_count, q);
      uint8_t* d_mask = sycl::malloc_device<uint8_t>(pixel_count, q);
      if (!d_out || !d_mask) {
        for (auto* p : d_bands)
          sycl::free(p, q);
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

      // Copy band pointers to device — we only support up to 8
      // bands in the kernel (fits in a fixed-size array, avoids
      // extra pointer indirection through device memory).
      constexpr size_t MAX_BANDS = PGACCEL_MAP_ALGEBRA_MAX_BANDS;
      if (band_count > MAX_BANDS) {
        for (auto* p : d_bands)
          sycl::free(p, q);
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
      for (size_t b = 0; b < MAX_BANDS; ++b)
        ba.p[b] = band_ptrs[b];

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
             band_vals[b] = ba.p[b][i];
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

      for (auto* p : d_bands)
        sycl::free(p, q);
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
                                              void* output_pixels, uint8_t* nodata_mask) {
  if (band_pixels == nullptr || expr == nullptr || output_pixels == nullptr) {
    return PGACCEL_ERROR_INIT;
  }
  if (expr->instructions == nullptr && expr->inst_count > 0) {
    return PGACCEL_ERROR_INIT;
  }

  return map_algebra_gpu(band_pixels, pixel_count, pixel_type, expr, output_pixels, nodata_mask);
}

extern "C" pgaccel_status pgaccel_raster_clip(const void* rast_pixels, size_t width, size_t height,
                                              double origin_x, double origin_y, double scale_x,
                                              double scale_y, int pixel_type,
                                              const float* clip_ring_xy, size_t vertex_count,
                                              void* output_pixels, uint8_t* nodata_mask) {
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
  } catch (const std::exception&) {
  } catch (...) {}
  return PGACCEL_ERROR_NO_DEVICE;
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
                                                  float* dst_pixels) {
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
                                               float* slope_out) {
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
}

/* ── Raster Aspect (3×3 gradient → compass direction) ─────────── */
/*
 * Per-pixel aspect (compass direction of the steepest descent), in degrees
 * [0, 360). North = 0, East = 90, South = 180, West = 270. Flat areas
 * (zero gradient) get -1 by convention (matching gdaldem). Edge pixels
 * are also -1.
 */
extern "C" pgaccel_status pgaccel_raster_aspect(const float* src_pixels, size_t width,
                                                size_t height, float* aspect_out) {
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
                                                   float* shade_out) {
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
                                               double* output) {
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
                                                      const uint8_t* nodata_masks, double* output) {
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
}

extern "C" pgaccel_status pgaccel_raster_reclass(const void* input_pixels, size_t pixel_count,
                                                 int input_type, const pgaccel_reclass_rule* rules,
                                                 size_t rule_count, int output_type,
                                                 void* output_pixels) {
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
  } catch (const std::exception&) {
  } catch (...) {}
  return PGACCEL_ERROR_NO_DEVICE;
}

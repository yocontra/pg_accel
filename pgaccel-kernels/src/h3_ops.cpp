#include <sycl/sycl.hpp>

#include <cmath>
#include <cstdio>
#include <cstring>
#include <limits>
#include <new>
#include <stdexcept>

#include "pgaccel_ffi.h"
#include "pgaccel_resident_count.h"
#include "pgaccel_queue.h"

#include "h3_exact_device.hpp"
#include "h3_float_device.hpp"

// SAFETY: g_queue is owned by device_manager.cpp. All H3 kernels share it so
// pgaccel_shutdown() tears down one Metal context for the process.

static sycl::queue& get_queue() {
  return pgaccel_require_queue();
}

class H3UsmAllocationGuard {
 public:
  H3UsmAllocationGuard(sycl::queue& queue, void* allocation) noexcept
      : queue_(&queue), allocation_(allocation) {}

  H3UsmAllocationGuard(const H3UsmAllocationGuard&) = delete;
  H3UsmAllocationGuard& operator=(const H3UsmAllocationGuard&) = delete;

  ~H3UsmAllocationGuard() noexcept {
    try {
      free_now();
    } catch (const std::exception& error) {
      std::fprintf(stderr, "pgaccel: H3 USM cleanup failed: %s\n", error.what());
    } catch (...) {
      std::fprintf(stderr, "pgaccel: H3 USM cleanup failed: unknown C++ exception\n");
    }
  }

  void free_now() {
    void* allocation = allocation_;
    allocation_ = nullptr;
    if (allocation != nullptr)
      sycl::free(allocation, *queue_);
  }

 private:
  sycl::queue* queue_;
  void* allocation_;
};

// ---------------------------------------------------------------------------
// H3 bit-layout constants
// ---------------------------------------------------------------------------
// Cell ID layout (64 bits, high to low) — matches the H3 v4 reference
// (h3_internal.h::`H3Index`):
//   [63]    = high bit, always zero for valid cell
//   [62-59] = mode (4 bits, 1 = cell)
//   [58-56] = reserved (3 bits, must be 0 for cells)
//   [55-52] = resolution (4 bits, 0-15)
//   [51-45] = base cell (7 bits, 0-121)
//   [44- 0] = 15 digit slots × 3 bits each (digits 0-6; 7 = unused)
//
// NOTE: digit slot `r` (1-indexed from resolution 1) lives at bits
//       [(15 - r) * 3 + 2 .. (15 - r) * 3] — i.e. shift = (15 - r) * 3.
//       Earlier revisions used a `+1` offset on the assumption bit 0 was
//       reserved; that does NOT match H3 v4 and caused digit-1 to overlap
//       with bit 45 of the base cell, silently corrupting `get_base_cell`,
//       `is_pentagon`, and `cell_to_center_child` results on real h3-pg
//       input. Layout aligned with the H3 reference in commit 2026-05-01.
// ---------------------------------------------------------------------------

static constexpr int H3_MAX_RESOLUTION = 15;
static constexpr uint64_t H3_MODE_CELL = 1ULL;
static constexpr uint64_t H3_HIGH_BIT = 1ULL << 63;
static constexpr uint64_t H3_RES_MASK = 0xFULL << 52;
static constexpr uint64_t H3_BASE_MASK = 0x7FULL << 45;
static constexpr uint64_t H3_DIGIT_MASK = 7ULL;
static constexpr uint64_t H3_UNUSED_DIGIT = 7ULL;
// H3 v4 pentagon base cells: {4, 14, 24, 38, 49, 58, 63, 72, 83, 97, 107, 117}.
static constexpr uint64_t H3_PENTAGON_BASE_LOW = (1ULL << 4) | (1ULL << 14) | (1ULL << 24) |
                                                 (1ULL << 38) | (1ULL << 49) | (1ULL << 58) |
                                                 (1ULL << 63);
static constexpr uint64_t H3_PENTAGON_BASE_HIGH =
    (1ULL << 8) | (1ULL << 19) | (1ULL << 33) | (1ULL << 43) | (1ULL << 53);

static inline bool h3_needs_exact_latlng_fixup(int resolution, uint8_t valid_flag) {
  // High resolutions need exact correction for every row. Lower resolutions
  // use the fp32 edge detector's `2` marker to correct only boundary-risk
  // rows before exposing cells to callers or grouped-count keys.
  return valid_flag == 2 || resolution >= 8;
}

static inline pgaccel_h3_exact::FaceIJK h3_empty_exact_face_ijk() {
  pgaccel_h3_exact::FaceIJK out;
  out.face = 0;
  out.coord.i = 0;
  out.coord.j = 0;
  out.coord.k = 0;
  return out;
}

static inline bool h3_lat_lng_bounds_valid(double lat_deg, double lng_deg) {
  return lat_deg >= -90.0 && lat_deg <= 90.0 && lng_deg >= -180.0 && lng_deg <= 180.0;
}

static inline uint8_t h3_exact_project_face_ijk(double lat_deg, double lng_deg, int res,
                                                pgaccel_h3_exact::FaceIJK& out) {
  if (res < 0 || res > H3_MAX_RESOLUTION || !h3_lat_lng_bounds_valid(lat_deg, lng_deg)) {
    out = h3_empty_exact_face_ijk();
    return 0;
  }
  pgaccel_h3_exact::LatLng g;
  g.lat = lat_deg * pgaccel_h3_exact::M_PI_180;
  g.lng = lng_deg * pgaccel_h3_exact::M_PI_180;
  out = pgaccel_h3_exact::geo_to_face_ijk(g, res);
  return 1;
}

static inline void h3_exact_finalize_face_ijk(const pgaccel_h3_exact::FaceIJK& fijk, int res,
                                              uint64_t& cell, uint8_t& valid) {
  const pgaccel_h3_exact::H3Index out = pgaccel_h3_exact::face_ijk_to_h3(fijk, res);
  cell = out;
  valid = out == 0 ? uint8_t(0) : uint8_t(1);
}

struct H3LatLngCellSlabLayout {
  size_t lat64_off;
  size_t lng64_off;
  size_t out_off;
  size_t face_ijk_off;
  size_t valid_off;
  size_t invalid_off;
  size_t lat32_off;
  size_t lng32_off;
  size_t slab_bytes;
};

static inline size_t h3_align_up_size(size_t value, size_t alignment) {
  return ((value + alignment - 1) / alignment) * alignment;
}

static inline H3LatLngCellSlabLayout h3_lat_lng_cell_slab_layout(size_t count) {
  const size_t f64_bytes = count * sizeof(double);
  const size_t out_bytes = count * sizeof(uint64_t);
  const size_t face_ijk_bytes = count * sizeof(pgaccel_h3_exact::FaceIJK);
  const size_t valid_bytes = count * sizeof(uint8_t);
  const size_t f32_bytes = count * sizeof(float);

  H3LatLngCellSlabLayout layout;
  layout.lat64_off = 0;
  layout.lng64_off = layout.lat64_off + f64_bytes;
  layout.out_off = layout.lng64_off + f64_bytes;
  layout.face_ijk_off =
      h3_align_up_size(layout.out_off + out_bytes, alignof(pgaccel_h3_exact::FaceIJK));
  layout.valid_off = layout.face_ijk_off + face_ijk_bytes;
  layout.invalid_off = h3_align_up_size(layout.valid_off + valid_bytes, alignof(uint32_t));
  layout.lat32_off = h3_align_up_size(layout.invalid_off + sizeof(uint32_t), alignof(float));
  layout.lng32_off = layout.lat32_off + f32_bytes;
  layout.slab_bytes = layout.lng32_off + f32_bytes;
  return layout;
}

static void h3_zero_lat_lng_cell_slab(uint8_t* slab, size_t count) {
  const H3LatLngCellSlabLayout layout = h3_lat_lng_cell_slab_layout(count);
  std::memset(slab + layout.out_off, 0, count * sizeof(uint64_t));
  std::memset(slab + layout.face_ijk_off, 0, count * sizeof(pgaccel_h3_exact::FaceIJK));
  std::memset(slab + layout.valid_off, 0, count * sizeof(uint8_t));
  std::memset(slab + layout.invalid_off, 0, sizeof(uint32_t));
}

static void h3_run_fast_f32_to_common_slab(sycl::queue& q, uint8_t* d_slab, size_t count, int res) {
  const size_t row_count = count;
  q.submit([&](sycl::handler& h) {
     h.parallel_for(sycl::range<1>(count), [=](sycl::id<1> id) {
       const size_t i = id[0];
       const H3LatLngCellSlabLayout layout = h3_lat_lng_cell_slab_layout(row_count);

       const float* d_lats = reinterpret_cast<const float*>(d_slab + layout.lat32_off);
       const float* d_lngs = reinterpret_cast<const float*>(d_slab + layout.lng32_off);
       auto* d_out = reinterpret_cast<uint64_t*>(d_slab + layout.out_off);
       uint8_t* d_valid = d_slab + layout.valid_off;

       const float lat_deg = d_lats[i];
       const float lng_deg = d_lngs[i];
       if (res < 0 || res > H3_MAX_RESOLUTION || lat_deg < -90.0f || lat_deg > 90.0f ||
           lng_deg < -180.0f || lng_deg > 180.0f) {
         d_valid[i] = 0;
         d_out[i] = 0;
         return;
       }

       const auto result = pgaccel_h3_float::lat_lng_to_cell_degs(lat_deg, lng_deg, res);
       d_valid[i] = result.valid == 0 ? 0 : (result.needs_fixup ? 2 : 1);
       d_out[i] = result.cell;
     });
   }).wait_and_throw();
}

static void h3_run_exact_split_to_common_slab(sycl::queue& q, uint8_t* d_slab, size_t count,
                                              int res, bool fix_all) {
  const size_t row_count = count;
  uint8_t fix_every = 0;
  if (fix_all)
    fix_every = 1;

  q.submit([&](sycl::handler& h) {
     h.parallel_for(sycl::range<1>(count), [=](sycl::id<1> id) {
       const size_t i = id[0];
       const H3LatLngCellSlabLayout layout = h3_lat_lng_cell_slab_layout(row_count);

       const double* d_lats = reinterpret_cast<const double*>(d_slab + layout.lat64_off);
       const double* d_lngs = reinterpret_cast<const double*>(d_slab + layout.lng64_off);
       auto* d_fijk = reinterpret_cast<pgaccel_h3_exact::FaceIJK*>(d_slab + layout.face_ijk_off);
       auto* d_out = reinterpret_cast<uint64_t*>(d_slab + layout.out_off);
       uint8_t* d_valid = d_slab + layout.valid_off;

       if (fix_every == 0 && !h3_needs_exact_latlng_fixup(res, d_valid[i]))
         return;

       pgaccel_h3_exact::FaceIJK projected;
       if (h3_exact_project_face_ijk(d_lats[i], d_lngs[i], res, projected) == 0) {
         d_valid[i] = 0;
         d_out[i] = 0;
         d_fijk[i] = h3_empty_exact_face_ijk();
         return;
       }
       d_fijk[i] = projected;
       d_valid[i] = 3;
     });
   }).wait_and_throw();

  q.submit([&](sycl::handler& h) {
     h.parallel_for(sycl::range<1>(count), [=](sycl::id<1> id) {
       const size_t i = id[0];
       const H3LatLngCellSlabLayout layout = h3_lat_lng_cell_slab_layout(row_count);

       const auto* d_fijk =
           reinterpret_cast<const pgaccel_h3_exact::FaceIJK*>(d_slab + layout.face_ijk_off);
       auto* d_out = reinterpret_cast<uint64_t*>(d_slab + layout.out_off);
       uint8_t* d_valid = d_slab + layout.valid_off;

       if (d_valid[i] != 3)
         return;

       uint64_t cell = 0;
       uint8_t valid_cell = 0;
       h3_exact_finalize_face_ijk(d_fijk[i], res, cell, valid_cell);
       d_out[i] = cell;
       d_valid[i] = valid_cell;
     });
   }).wait_and_throw();
}

static void h3_validate_common_slab_keys(sycl::queue& q, uint8_t* d_slab, size_t count) {
  const size_t row_count = count;
  q.submit([&](sycl::handler& h) {
     h.parallel_for(sycl::range<1>(count), [=](sycl::id<1> id) {
       const size_t i = id[0];
       const H3LatLngCellSlabLayout layout = h3_lat_lng_cell_slab_layout(row_count);

       const auto* d_out = reinterpret_cast<const uint64_t*>(d_slab + layout.out_off);
       const uint8_t* d_valid = d_slab + layout.valid_off;
       uint32_t* d_invalid = reinterpret_cast<uint32_t*>(d_slab + layout.invalid_off);

       if (d_valid[i] == 0 || d_out[i] == 0) {
         sycl::atomic_ref<uint32_t, sycl::memory_order::relaxed, sycl::memory_scope::device,
                          sycl::access::address_space::global_space>
             invalid_ref(d_invalid[0]);
         invalid_ref.store(1u);
       }
     });
   }).wait_and_throw();
}

static void h3_run_fast_f32_device_to_common_slab(sycl::queue& q, uint8_t* d_slab,
                                                  const float* d_input_lats,
                                                  const float* d_input_lngs, size_t count,
                                                  int res) {
  const size_t row_count = count;
  q.submit([&](sycl::handler& h) {
     h.parallel_for(sycl::range<1>(count), [=](sycl::id<1> id) {
       const size_t i = id[0];
       const H3LatLngCellSlabLayout layout = h3_lat_lng_cell_slab_layout(row_count);

       auto* d_out = reinterpret_cast<uint64_t*>(d_slab + layout.out_off);
       uint8_t* d_valid = d_slab + layout.valid_off;

       const float lat_deg = d_input_lats[i];
       const float lng_deg = d_input_lngs[i];
       if (res < 0 || res > H3_MAX_RESOLUTION || lat_deg < -90.0f || lat_deg > 90.0f ||
           lng_deg < -180.0f || lng_deg > 180.0f) {
         d_valid[i] = 0;
         d_out[i] = 0;
         return;
       }

       const auto result = pgaccel_h3_float::lat_lng_to_cell_degs(lat_deg, lng_deg, res);
       d_valid[i] = result.valid == 0 ? 0 : (result.needs_fixup ? 2 : 1);
       d_out[i] = result.cell;
     });
   }).wait_and_throw();
}

static void h3_run_exact_split_device_to_common_slab(sycl::queue& q, uint8_t* d_slab,
                                                     const double* d_input_lats,
                                                     const double* d_input_lngs, size_t count,
                                                     int res, bool fix_all) {
  const size_t row_count = count;

  if (fix_all) {
    q.submit([&](sycl::handler& h) {
       h.parallel_for(sycl::range<1>(count), [=](sycl::id<1> id) {
         const size_t i = id[0];
         const H3LatLngCellSlabLayout layout = h3_lat_lng_cell_slab_layout(row_count);

         auto* d_fijk = reinterpret_cast<pgaccel_h3_exact::FaceIJK*>(d_slab + layout.face_ijk_off);
         auto* d_out = reinterpret_cast<uint64_t*>(d_slab + layout.out_off);
         uint8_t* d_valid = d_slab + layout.valid_off;

         pgaccel_h3_exact::FaceIJK projected;
         if (h3_exact_project_face_ijk(d_input_lats[i], d_input_lngs[i], res, projected) == 0) {
           d_valid[i] = 0;
           d_out[i] = 0;
           d_fijk[i] = h3_empty_exact_face_ijk();
           return;
         }
         d_fijk[i] = projected;
         d_valid[i] = 3;
       });
     }).wait_and_throw();
  } else {
    q.submit([&](sycl::handler& h) {
       h.parallel_for(sycl::range<1>(count), [=](sycl::id<1> id) {
         const size_t i = id[0];
         const H3LatLngCellSlabLayout layout = h3_lat_lng_cell_slab_layout(row_count);

         auto* d_fijk = reinterpret_cast<pgaccel_h3_exact::FaceIJK*>(d_slab + layout.face_ijk_off);
         auto* d_out = reinterpret_cast<uint64_t*>(d_slab + layout.out_off);
         uint8_t* d_valid = d_slab + layout.valid_off;

         if (!h3_needs_exact_latlng_fixup(res, d_valid[i]))
           return;

         pgaccel_h3_exact::FaceIJK projected;
         if (h3_exact_project_face_ijk(d_input_lats[i], d_input_lngs[i], res, projected) == 0) {
           d_valid[i] = 0;
           d_out[i] = 0;
           d_fijk[i] = h3_empty_exact_face_ijk();
           return;
         }
         d_fijk[i] = projected;
         d_valid[i] = 3;
       });
     }).wait_and_throw();
  }

  q.submit([&](sycl::handler& h) {
     h.parallel_for(sycl::range<1>(count), [=](sycl::id<1> id) {
       const size_t i = id[0];
       const H3LatLngCellSlabLayout layout = h3_lat_lng_cell_slab_layout(row_count);

       const auto* d_fijk =
           reinterpret_cast<const pgaccel_h3_exact::FaceIJK*>(d_slab + layout.face_ijk_off);
       auto* d_out = reinterpret_cast<uint64_t*>(d_slab + layout.out_off);
       uint8_t* d_valid = d_slab + layout.valid_off;

       if (d_valid[i] != 3)
         return;

       uint64_t cell = 0;
       uint8_t valid_cell = 0;
       h3_exact_finalize_face_ijk(d_fijk[i], res, cell, valid_cell);
       d_out[i] = cell;
       d_valid[i] = valid_cell;
     });
   }).wait_and_throw();
}

// ---------------------------------------------------------------------------
// Inline helpers
// ---------------------------------------------------------------------------

static inline int32_t h3_get_resolution(uint64_t cell) {
  return static_cast<int32_t>((cell >> 52) & 0xF);
}

static inline int32_t h3_get_base_cell(uint64_t cell) {
  return static_cast<int32_t>((cell >> 45) & 0x7F);
}

static inline int32_t h3_get_digit(uint64_t cell, int res) {
  // res is 1-based resolution index (1..15)
  int shift = (H3_MAX_RESOLUTION - res) * 3;
  return static_cast<int32_t>((cell >> shift) & H3_DIGIT_MASK);
}

static inline bool h3_is_pentagon_base(int32_t base) {
  return base < 64 ? ((H3_PENTAGON_BASE_LOW >> base) & 1ULL) != 0
                   : ((H3_PENTAGON_BASE_HIGH >> (base - 64)) & 1ULL) != 0;
}

static inline bool h3_is_valid_cell(uint64_t cell) {
  if (cell == 0)
    return false;
  // H3 cell indexes reserve the high bit and require it to be zero.
  if ((cell & H3_HIGH_BIT) != 0)
    return false;
  // Reserved bits must be zero for cells.
  if (((cell >> 56) & 0x7) != 0)
    return false;
  // Mode must be 1 (cell)
  uint64_t mode = (cell >> 59) & 0xF;
  if (mode != H3_MODE_CELL)
    return false;
  int res = h3_get_resolution(cell);
  if (res < 0 || res > H3_MAX_RESOLUTION)
    return false;
  int base = h3_get_base_cell(cell);
  if (base < 0 || base > 121)
    return false;
  bool found_nonzero_digit = false;
  const bool pentagon_base = h3_is_pentagon_base(base);
  for (int r = 1; r <= res; r++) {
    const int digit = h3_get_digit(cell, r);
    if (digit == H3_UNUSED_DIGIT)
      return false;
    // H3's deleted K-axis subsequence is invalid beneath a pentagon base.
    if (pentagon_base && !found_nonzero_digit && digit == 1)
      return false;
    found_nonzero_digit = found_nonzero_digit || digit != 0;
  }
  // Digits beyond resolution must be 7 (unused)
  for (int r = res + 1; r <= H3_MAX_RESOLUTION; r++) {
    if (h3_get_digit(cell, r) != 7)
      return false;
  }
  return true;
}

static inline uint64_t h3_unused_digit_mask_after(int parent_res) {
  uint64_t mask = 0;
  for (int r = parent_res + 1; r <= H3_MAX_RESOLUTION; r++) {
    const int shift = (H3_MAX_RESOLUTION - r) * 3;
    mask |= (H3_UNUSED_DIGIT << shift);
  }
  return mask;
}

static inline uint64_t h3_cell_to_parent_masked(uint64_t cell, int parent_res,
                                                uint64_t unused_digit_mask) {
  const int res = h3_get_resolution(cell);
  if (parent_res < 0 || parent_res > res)
    return 0;
  if (parent_res == res)
    return cell;

  uint64_t result = cell;
  result = (result & ~H3_RES_MASK) | (static_cast<uint64_t>(parent_res) << 52);
  result |= unused_digit_mask;
  return result;
}

static inline size_t h3_cell_count_at_resolution(int resolution) {
  size_t pow7 = 1;
  for (int r = 0; r < resolution; ++r)
    pow7 *= 7;
  return 2 + 120 * pow7;
}

// ---------------------------------------------------------------------------
// Extern "C" API — GPU-only (no CPU fallback, per rule #11)
// ---------------------------------------------------------------------------

extern "C" pgaccel_status pgaccel_h3_get_resolution_bulk(const uint64_t* cells, size_t count,
                                                         int32_t* resolutions) try {
  if (count == 0)
    return PGACCEL_OK;
  if (cells == nullptr || resolutions == nullptr)
    return PGACCEL_ERROR_INIT;

  try {
    sycl::queue& q = get_queue();

    // Shared USM avoids Metal's cold queue::memcpy blit path before the
    // first tiny H3 kernel launch.
    uint64_t* d_cells = sycl::malloc_shared<uint64_t>(count, q);
    int32_t* d_res = sycl::malloc_shared<int32_t>(count, q);

    if (!d_cells || !d_res) {
      sycl::free(d_cells, q);
      sycl::free(d_res, q);
      return PGACCEL_ERROR_OOM;
    }

    std::memcpy(d_cells, cells, count * sizeof(uint64_t));

    q.submit([&](sycl::handler& h) {
       h.parallel_for(sycl::range<1>(count), [=](sycl::id<1> id) {
         const size_t i = id[0];
         if (d_cells[i] == 0) {
           d_res[i] = -1;
         } else {
           // Inline resolution extraction: bits [55:52]
           d_res[i] = static_cast<int32_t>((d_cells[i] >> 52) & 0xF);
         }
       });
     }).wait_and_throw();

    std::memcpy(resolutions, d_res, count * sizeof(int32_t));

    sycl::free(d_cells, q);
    sycl::free(d_res, q);
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
  return pgaccel_kernel_failure("pgaccel_h3_get_resolution_bulk", &e);
} catch (...) {
  return pgaccel_kernel_failure("pgaccel_h3_get_resolution_bulk", nullptr);
}

extern "C" pgaccel_status pgaccel_h3_get_base_cell_bulk(const uint64_t* cells, size_t count,
                                                        int32_t* base_cells) try {
  if (count == 0)
    return PGACCEL_OK;
  if (cells == nullptr || base_cells == nullptr)
    return PGACCEL_ERROR_INIT;

  try {
    sycl::queue& q = get_queue();

    uint64_t* d_cells = sycl::malloc_shared<uint64_t>(count, q);
    int32_t* d_base = sycl::malloc_shared<int32_t>(count, q);

    if (!d_cells || !d_base) {
      sycl::free(d_cells, q);
      sycl::free(d_base, q);
      return PGACCEL_ERROR_OOM;
    }

    std::memcpy(d_cells, cells, count * sizeof(uint64_t));

    q.submit([&](sycl::handler& h) {
       h.parallel_for(sycl::range<1>(count), [=](sycl::id<1> id) {
         const size_t i = id[0];
         if (d_cells[i] == 0) {
           d_base[i] = -1;
         } else {
           // Inline base-cell extraction: bits [51:45].
           d_base[i] = static_cast<int32_t>((d_cells[i] >> 45) & 0x7F);
         }
       });
     }).wait_and_throw();

    std::memcpy(base_cells, d_base, count * sizeof(int32_t));

    sycl::free(d_cells, q);
    sycl::free(d_base, q);
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
  return pgaccel_kernel_failure("pgaccel_h3_get_base_cell_bulk", &e);
} catch (...) {
  return pgaccel_kernel_failure("pgaccel_h3_get_base_cell_bulk", nullptr);
}

extern "C" pgaccel_status pgaccel_h3_is_valid_cell_bulk(const uint64_t* cells, size_t count,
                                                        uint8_t* valid) try {
  if (count == 0)
    return PGACCEL_OK;
  if (cells == nullptr || valid == nullptr)
    return PGACCEL_ERROR_INIT;

  try {
    sycl::queue& q = get_queue();

    uint64_t* d_cells = sycl::malloc_shared<uint64_t>(count, q);
    uint8_t* d_valid = sycl::malloc_shared<uint8_t>(count, q);

    if (!d_cells || !d_valid) {
      sycl::free(d_cells, q);
      sycl::free(d_valid, q);
      return PGACCEL_ERROR_OOM;
    }

    std::memcpy(d_cells, cells, count * sizeof(uint64_t));

    const uint64_t high_bit = H3_HIGH_BIT;
    const uint64_t mode_cell = H3_MODE_CELL;
    const int max_res = H3_MAX_RESOLUTION;
    const uint64_t unused_digit = H3_UNUSED_DIGIT;

    q.submit([&](sycl::handler& h) {
       h.parallel_for(sycl::range<1>(count), [=](sycl::id<1> id) {
         const size_t i = id[0];
         const uint64_t cell = d_cells[i];

         if (cell == 0) {
           d_valid[i] = 0;
           return;
         }
         // High bit is reserved and must be zero for H3 cells.
         if ((cell & high_bit) != 0) {
           d_valid[i] = 0;
           return;
         }
         // Reserved bits [58:56] must be zero for cells.
         if (((cell >> 56) & 0x7) != 0) {
           d_valid[i] = 0;
           return;
         }
         // Mode (bits 62:59) must be 1 = cell.
         const uint64_t mode = (cell >> 59) & 0xF;
         if (mode != mode_cell) {
           d_valid[i] = 0;
           return;
         }
         // Resolution (bits 55:52) in [0, 15].
         const int res = static_cast<int>((cell >> 52) & 0xF);
         if (res < 0 || res > max_res) {
           d_valid[i] = 0;
           return;
         }
         // Base cell (bits 51:45) in [0, 121].
         const int base = static_cast<int>((cell >> 45) & 0x7F);
         if (base < 0 || base > 121) {
           d_valid[i] = 0;
           return;
         }
         // Digits beyond resolution must be 7 (unused).
         bool ok = true;
         for (int r = res + 1; r <= max_res; r++) {
           const int shift = (max_res - r) * 3;
           const uint64_t digit = (cell >> shift) & 0x7;
           if (digit != unused_digit) {
             ok = false;
             break;
           }
         }
         d_valid[i] = ok ? 1 : 0;
       });
     }).wait_and_throw();

    std::memcpy(valid, d_valid, count * sizeof(uint8_t));

    sycl::free(d_cells, q);
    sycl::free(d_valid, q);
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
  return pgaccel_kernel_failure("pgaccel_h3_is_valid_cell_bulk", &e);
} catch (...) {
  return pgaccel_kernel_failure("pgaccel_h3_is_valid_cell_bulk", nullptr);
}

extern "C" pgaccel_status pgaccel_h3_is_pentagon_bulk(const uint64_t* cells, size_t count,
                                                      uint8_t* is_pent) try {
  if (count == 0)
    return PGACCEL_OK;
  if (cells == nullptr || is_pent == nullptr)
    return PGACCEL_ERROR_INIT;

  try {
    sycl::queue& q = get_queue();

    uint64_t* d_cells = sycl::malloc_shared<uint64_t>(count, q);
    uint8_t* d_pent = sycl::malloc_shared<uint8_t>(count, q);

    if (!d_cells || !d_pent) {
      sycl::free(d_cells, q);
      sycl::free(d_pent, q);
      return PGACCEL_ERROR_OOM;
    }

    std::memcpy(d_cells, cells, count * sizeof(uint64_t));

    const uint64_t pent_low = H3_PENTAGON_BASE_LOW;
    const uint64_t pent_high = H3_PENTAGON_BASE_HIGH;
    const int max_res = H3_MAX_RESOLUTION;

    q.submit([&](sycl::handler& h) {
       h.parallel_for(sycl::range<1>(count), [=](sycl::id<1> id) {
         const size_t i = id[0];
         const uint64_t cell = d_cells[i];

         if (cell == 0) {
           d_pent[i] = 0;
           return;
         }
         // Base cell membership test against the 12-pentagon set.
         const int base = static_cast<int>((cell >> 45) & 0x7F);
         const bool is_pent_base = (base < 64) ? ((pent_low >> base) & 1ULL) != 0
                                               : ((pent_high >> (base - 64)) & 1ULL) != 0;
         if (!is_pent_base) {
           d_pent[i] = 0;
           return;
         }
         // A cell is a pentagon iff its base cell is a pentagon AND the
         // leading non-zero digit is 0 — i.e. all sub-resolution digits are
         // center (digit 0). Mirrors `_h3LeadingNonZeroDigit == 0` in the
         // H3 reference's `isPentagon`.
         const int res = static_cast<int>((cell >> 52) & 0xF);
         bool all_zero = true;
         for (int r = 1; r <= res; r++) {
           const int shift = (max_res - r) * 3;
           const uint64_t digit = (cell >> shift) & 0x7;
           if (digit != 0) {
             all_zero = false;
             break;
           }
         }
         d_pent[i] = all_zero ? 1 : 0;
       });
     }).wait_and_throw();

    std::memcpy(is_pent, d_pent, count * sizeof(uint8_t));

    sycl::free(d_cells, q);
    sycl::free(d_pent, q);
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
  return pgaccel_kernel_failure("pgaccel_h3_is_pentagon_bulk", &e);
} catch (...) {
  return pgaccel_kernel_failure("pgaccel_h3_is_pentagon_bulk", nullptr);
}

extern "C" pgaccel_status pgaccel_h3_is_res_class_iii_bulk(const uint64_t* cells, size_t count,
                                                           uint8_t* is_class_iii) try {
  if (count == 0)
    return PGACCEL_OK;
  if (cells == nullptr || is_class_iii == nullptr)
    return PGACCEL_ERROR_INIT;

  try {
    sycl::queue& q = get_queue();

    uint64_t* d_cells = sycl::malloc_shared<uint64_t>(count, q);
    uint8_t* d_out = sycl::malloc_shared<uint8_t>(count, q);

    if (!d_cells || !d_out) {
      sycl::free(d_cells, q);
      sycl::free(d_out, q);
      return PGACCEL_ERROR_OOM;
    }

    std::memcpy(d_cells, cells, count * sizeof(uint64_t));

    q.submit([&](sycl::handler& h) {
       h.parallel_for(sycl::range<1>(count), [=](sycl::id<1> id) {
         const size_t i = id[0];
         // Class III iff resolution is odd. Resolution lives in bits 55:52.
         const int res = static_cast<int>((d_cells[i] >> 52) & 0xF);
         d_out[i] = (res & 1) ? 1 : 0;
       });
     }).wait_and_throw();

    std::memcpy(is_class_iii, d_out, count * sizeof(uint8_t));

    sycl::free(d_cells, q);
    sycl::free(d_out, q);
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
  return pgaccel_kernel_failure("pgaccel_h3_is_res_class_iii_bulk", &e);
} catch (...) {
  return pgaccel_kernel_failure("pgaccel_h3_is_res_class_iii_bulk", nullptr);
}

extern "C" pgaccel_status pgaccel_h3_cell_to_parent_bulk(const uint64_t* cells, size_t count,
                                                         int parent_res, uint64_t* parents) try {
  if (count == 0)
    return PGACCEL_OK;
  if (cells == nullptr || parents == nullptr)
    return PGACCEL_ERROR_INIT;
  if (parent_res < 0 || parent_res > H3_MAX_RESOLUTION) {
    return PGACCEL_ERROR_UNSUPPORTED;
  }

  try {
    sycl::queue& q = get_queue();

    uint64_t* d_cells = sycl::malloc_shared<uint64_t>(count, q);
    uint64_t* d_parents = sycl::malloc_shared<uint64_t>(count, q);

    if (!d_cells || !d_parents) {
      sycl::free(d_cells, q);
      sycl::free(d_parents, q);
      return PGACCEL_ERROR_OOM;
    }

    std::memcpy(d_cells, cells, count * sizeof(uint64_t));

    const int p_res = parent_res;
    const uint64_t unused_digit = H3_UNUSED_DIGIT;
    const int max_res = H3_MAX_RESOLUTION;
    const uint64_t res_mask = H3_RES_MASK;

    q.submit([&](sycl::handler& h) {
       h.parallel_for(sycl::range<1>(count), [=](sycl::id<1> id) {
         const size_t i = id[0];
         uint64_t cell = d_cells[i];

         if (cell == 0) {
           d_parents[i] = 0;
           return;
         }

         int res = static_cast<int>((cell >> 52) & 0xF);
         if (p_res > res) {
           d_parents[i] = 0;
           return;
         }
         if (p_res == res) {
           d_parents[i] = cell;
           return;
         }

         // Set resolution field
         uint64_t result = (cell & ~res_mask) | (static_cast<uint64_t>(p_res) << 52);
         // Clear child digits — set to 7 (unused)
         for (int r = p_res + 1; r <= max_res; r++) {
           int shift = (max_res - r) * 3;
           result |= (unused_digit << shift);
         }
         d_parents[i] = result;
       });
     }).wait_and_throw();

    std::memcpy(parents, d_parents, count * sizeof(uint64_t));

    sycl::free(d_cells, q);
    sycl::free(d_parents, q);
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
  return pgaccel_kernel_failure("pgaccel_h3_cell_to_parent_bulk", &e);
} catch (...) {
  return pgaccel_kernel_failure("pgaccel_h3_cell_to_parent_bulk", nullptr);
}

struct H3ResidentSpan {
  uintptr_t begin;
  uintptr_t end;
};

static bool h3_resident_span(const void* pointer, size_t count, size_t width,
                             H3ResidentSpan* span) {
  if (pointer == nullptr || count == 0 || width == 0 ||
      count > std::numeric_limits<size_t>::max() / width)
    return false;
  const size_t bytes = count * width;
  const uintptr_t begin = reinterpret_cast<uintptr_t>(pointer);
  if (begin > std::numeric_limits<uintptr_t>::max() - bytes)
    return false;
  *span = {begin, begin + bytes};
  return true;
}

static bool h3_spans_overlap(const H3ResidentSpan& lhs, const H3ResidentSpan& rhs) {
  return lhs.begin < rhs.end && rhs.begin < lhs.end;
}

static bool h3_current_device_pointer(sycl::queue& queue, const void* pointer) {
  if (pointer == nullptr)
    return false;
  try {
    const sycl::usm::alloc allocation = sycl::get_pointer_type(pointer, queue.get_context());
    return (allocation == sycl::usm::alloc::device || allocation == sycl::usm::alloc::shared) &&
           sycl::get_pointer_device(pointer, queue.get_context()) == queue.get_device();
  } catch (...) {
    return false;
  }
}

class H3ResidentStatusOwner {
 public:
  H3ResidentStatusOwner(sycl::queue& queue, uint32_t* status) : queue_(queue), status_(status) {}
  H3ResidentStatusOwner(const H3ResidentStatusOwner&) = delete;
  H3ResidentStatusOwner& operator=(const H3ResidentStatusOwner&) = delete;
  ~H3ResidentStatusOwner() {
    if (status_ != nullptr)
      sycl::free(status_, queue_);
  }

 private:
  sycl::queue& queue_;
  uint32_t* status_;
};

class H3CellToParentResidentValidateKernel;
class H3CellToParentResidentTransformKernel;

static constexpr uint32_t H3_RESIDENT_FAILURE_CONTRACT = 1u << 0;
static constexpr uint32_t H3_RESIDENT_FAILURE_INVALID_CELL = 1u << 1;
static constexpr uint32_t H3_RESIDENT_FAILURE_RES_MISMATCH = 1u << 2;

extern "C" pgaccel_status
pgaccel_h3_cell_to_parent_resident_ex(const uint64_t* cells, const uint8_t* nulls, size_t count,
                                      int32_t parent_res, uint64_t* parents, int32_t* detail) try {
  if (detail == nullptr)
    return PGACCEL_INVALID_ARGUMENT;
  *detail = PGACCEL_H3_PARENT_DETAIL_NONE;
  if (parent_res < 0 || parent_res > H3_MAX_RESOLUTION) {
    *detail = PGACCEL_H3_PARENT_DETAIL_CONTRACT;
    return PGACCEL_INVALID_ARGUMENT;
  }
  if (count == 0)
    return PGACCEL_OK;
  if (cells == nullptr || parents == nullptr) {
    *detail = PGACCEL_H3_PARENT_DETAIL_CONTRACT;
    return PGACCEL_INVALID_ARGUMENT;
  }

  H3ResidentSpan cells_span{};
  H3ResidentSpan parents_span{};
  H3ResidentSpan nulls_span{};
  if (!h3_resident_span(cells, count, sizeof(uint64_t), &cells_span) ||
      !h3_resident_span(parents, count, sizeof(uint64_t), &parents_span) ||
      h3_spans_overlap(cells_span, parents_span) ||
      (nulls != nullptr &&
       (!h3_resident_span(nulls, count, sizeof(uint8_t), &nulls_span) ||
        h3_spans_overlap(nulls_span, cells_span) || h3_spans_overlap(nulls_span, parents_span)))) {
    *detail = PGACCEL_H3_PARENT_DETAIL_CONTRACT;
    return PGACCEL_INVALID_ARGUMENT;
  }

  sycl::queue& queue = get_queue();
  if (!h3_current_device_pointer(queue, cells) || !h3_current_device_pointer(queue, parents) ||
      (nulls != nullptr && !h3_current_device_pointer(queue, nulls))) {
    *detail = PGACCEL_H3_PARENT_DETAIL_CONTRACT;
    return PGACCEL_INVALID_ARGUMENT;
  }

  uint32_t* failure_flags = sycl::malloc_shared<uint32_t>(1, queue);
  if (failure_flags == nullptr)
    return PGACCEL_OOM;
  H3ResidentStatusOwner status_owner(queue, failure_flags);
  *failure_flags = 0;

  const int32_t target_resolution = parent_res;
  queue.parallel_for<H3CellToParentResidentValidateKernel>(
      sycl::range<1>(count), [=](sycl::id<1> id) {
        const size_t row = id[0];
        const uint8_t null_byte = nulls == nullptr ? 0 : nulls[row];
        uint32_t row_failure = null_byte > 1 ? H3_RESIDENT_FAILURE_CONTRACT : 0;
        if (row_failure == 0 && null_byte == 0) {
          const uint64_t cell = cells[row];
          if (!h3_is_valid_cell(cell))
            row_failure = H3_RESIDENT_FAILURE_INVALID_CELL;
          else if (target_resolution > h3_get_resolution(cell))
            row_failure = H3_RESIDENT_FAILURE_RES_MISMATCH;
        }
        if (row_failure != 0) {
          sycl::atomic_ref<uint32_t, sycl::memory_order::relaxed, sycl::memory_scope::device,
                           sycl::access::address_space::global_space>
              failure_ref(*failure_flags);
          failure_ref.fetch_or(row_failure);
        }
      });
  queue.wait_and_throw();
  if (*failure_flags != 0) {
    pgaccel_record_gpu_exec();
    if ((*failure_flags & H3_RESIDENT_FAILURE_CONTRACT) != 0)
      *detail = PGACCEL_H3_PARENT_DETAIL_CONTRACT;
    else if ((*failure_flags & H3_RESIDENT_FAILURE_INVALID_CELL) != 0)
      *detail = PGACCEL_H3_PARENT_DETAIL_INVALID_CELL;
    else
      *detail = PGACCEL_H3_PARENT_DETAIL_RES_MISMATCH;
    return PGACCEL_INVALID_ARGUMENT;
  }

  const uint64_t unused_digit_mask = h3_unused_digit_mask_after(parent_res);
  queue.parallel_for<H3CellToParentResidentTransformKernel>(
      sycl::range<1>(count), [=](sycl::id<1> id) {
        const size_t row = id[0];
        if (nulls != nullptr && nulls[row] != 0) {
          parents[row] = 0;
          return;
        }
        parents[row] = h3_cell_to_parent_masked(cells[row], target_resolution, unused_digit_mask);
      });
  queue.wait_and_throw();
  pgaccel_record_gpu_exec();
  return PGACCEL_OK;
} catch (const pgaccel_no_device_error&) {
  return PGACCEL_ERROR_NO_DEVICE;
} catch (const std::bad_alloc&) {
  return PGACCEL_OOM;
} catch (const std::exception& error) {
  return pgaccel_kernel_failure("pgaccel_h3_cell_to_parent_resident_ex", &error);
} catch (...) {
  return pgaccel_kernel_failure("pgaccel_h3_cell_to_parent_resident_ex", nullptr);
}

extern "C" pgaccel_status pgaccel_h3_cell_to_parent_resident(const uint64_t* cells,
                                                             const uint8_t* nulls, size_t count,
                                                             int32_t parent_res,
                                                             uint64_t* parents) {
  int32_t detail = PGACCEL_H3_PARENT_DETAIL_NONE;
  return pgaccel_h3_cell_to_parent_resident_ex(cells, nulls, count, parent_res, parents, &detail);
}

extern "C" pgaccel_status pgaccel_h3_cell_to_parent_count_bulk(const uint64_t* cells, size_t count,
                                                               int parent_res,
                                                               pgaccel_agg_state** out_state) try {
  if (out_state == nullptr)
    return PGACCEL_ERROR_INIT;
  *out_state = nullptr;
  if (count == 0)
    return PGACCEL_OK;
  if (cells == nullptr)
    return PGACCEL_ERROR_INIT;
  if (parent_res < 0 || parent_res > H3_MAX_RESOLUTION)
    return PGACCEL_ERROR_UNSUPPORTED;

  try {
    sycl::queue& q = get_queue();

    const size_t cell_bytes = count * sizeof(uint64_t);
    const size_t key_bytes = count * sizeof(int64_t);
    const size_t invalid_bytes = sizeof(uint32_t);

    const size_t cells_off = 0;
    const size_t keys_off = cells_off + cell_bytes;
    const size_t invalid_off = keys_off + key_bytes;
    const size_t slab_bytes = invalid_off + invalid_bytes;

    uint8_t* d_slab = sycl::malloc_shared<uint8_t>(slab_bytes, q);
    if (d_slab == nullptr)
      return PGACCEL_ERROR_OOM;
    H3UsmAllocationGuard slab_guard(q, d_slab);

    auto* d_cells = reinterpret_cast<uint64_t*>(d_slab + cells_off);
    auto* d_parent_keys = reinterpret_cast<int64_t*>(d_slab + keys_off);
    auto* d_invalid = reinterpret_cast<uint32_t*>(d_slab + invalid_off);

    std::memcpy(d_cells, cells, cell_bytes);
    std::memset(d_parent_keys, 0, key_bytes);
    *d_invalid = 0;

    const int p_res = parent_res;
    const uint64_t unused_digit_mask = h3_unused_digit_mask_after(parent_res);
    q.submit([&](sycl::handler& h) {
       h.parallel_for(sycl::range<1>(count), [=](sycl::id<1> id) {
         const size_t i = id[0];
         const uint64_t cell = d_cells[i];
         if (cell == 0 || !h3_is_valid_cell(cell)) {
           d_parent_keys[i] = 0;
           sycl::atomic_ref<uint32_t, sycl::memory_order::relaxed, sycl::memory_scope::device,
                            sycl::access::address_space::global_space>
               invalid_ref(d_invalid[0]);
           invalid_ref.store(1u);
           return;
         }

         const uint64_t parent = h3_cell_to_parent_masked(cell, p_res, unused_digit_mask);
         if (parent == 0) {
           d_parent_keys[i] = 0;
           sycl::atomic_ref<uint32_t, sycl::memory_order::relaxed, sycl::memory_scope::device,
                            sycl::access::address_space::global_space>
               invalid_ref(d_invalid[0]);
           invalid_ref.store(1u);
           return;
         }

         d_parent_keys[i] = static_cast<int64_t>(parent);
       });
     }).wait_and_throw();
    pgaccel_record_gpu_exec();

    if (*d_invalid != 0) {
      slab_guard.free_now();
      return PGACCEL_ERROR;
    }

    const size_t max_distinct = h3_cell_count_at_resolution(parent_res);
    pgaccel_agg_state* state = nullptr;
    const pgaccel_status count_status =
        pgaccel_hash_count_i64_device_hash_execute_bounded_checked(d_parent_keys, count,
                                                                   max_distinct, &state);
    if (count_status != PGACCEL_OK) {
      slab_guard.free_now();
      return count_status;
    }

    *out_state = state;
    slab_guard.free_now();
    return PGACCEL_OK;
  } catch (const pgaccel_no_device_error&) {
    return PGACCEL_ERROR_NO_DEVICE;
  } catch (const std::bad_alloc&) {
    return PGACCEL_ERROR_OOM;
  } catch (const std::exception& e) {
    std::fprintf(stderr, "pgaccel: h3_cell_to_parent_count_bulk failed: %s\n", e.what());
    return PGACCEL_ERROR;
  } catch (...) {
    std::fprintf(stderr, "pgaccel: h3_cell_to_parent_count_bulk failed (unknown)\n");
    return PGACCEL_ERROR;
  }
} catch (const pgaccel_no_device_error&) {
  return PGACCEL_ERROR_NO_DEVICE;
} catch (const std::exception& e) {
  return pgaccel_kernel_failure("pgaccel_h3_cell_to_parent_count_bulk", &e);
} catch (...) {
  return pgaccel_kernel_failure("pgaccel_h3_cell_to_parent_count_bulk", nullptr);
}

extern "C" pgaccel_status pgaccel_h3_cell_to_center_child_bulk(const uint64_t* cells, size_t count,
                                                               int child_res,
                                                               uint64_t* children) try {
  if (count == 0)
    return PGACCEL_OK;
  if (cells == nullptr || children == nullptr)
    return PGACCEL_ERROR_INIT;
  if (child_res < 0 || child_res > H3_MAX_RESOLUTION)
    return PGACCEL_ERROR_UNSUPPORTED;

  try {
    sycl::queue& q = get_queue();

    uint64_t* d_cells = sycl::malloc_shared<uint64_t>(count, q);
    uint64_t* d_children = sycl::malloc_shared<uint64_t>(count, q);

    if (!d_cells || !d_children) {
      sycl::free(d_cells, q);
      sycl::free(d_children, q);
      return PGACCEL_ERROR_OOM;
    }

    std::memcpy(d_cells, cells, count * sizeof(uint64_t));

    const int c_res = child_res;
    const uint64_t unused_digit = H3_UNUSED_DIGIT;
    const int max_res = H3_MAX_RESOLUTION;
    const uint64_t res_mask = H3_RES_MASK;

    q.submit([&](sycl::handler& h) {
       h.parallel_for(sycl::range<1>(count), [=](sycl::id<1> id) {
         const size_t i = id[0];
         const uint64_t cell = d_cells[i];

         if (cell == 0) {
           d_children[i] = 0;
           return;
         }

         const int res = static_cast<int>((cell >> 52) & 0xF);
         if (c_res < res) {
           // Cannot navigate to a coarser resolution from a finer cell.
           d_children[i] = 0;
           return;
         }
         if (c_res == res) {
           d_children[i] = cell;
           return;
         }

         // Set resolution field to child_res.
         uint64_t result = (cell & ~res_mask) | (static_cast<uint64_t>(c_res) << 52);
         // Clear digit slots in (res, c_res] — these are the "new" digits
         // for the descent path. Center child convention: each new digit
         // is 0. We set them by clearing their existing 7 (unused) value.
         for (int r = res + 1; r <= c_res; r++) {
           const int shift = (max_res - r) * 3;
           // First clear the 3 bits at this position (they currently
           // hold H3_UNUSED_DIGIT = 7), leaving 0 (= center child digit).
           result &= ~(uint64_t{0x7} << shift);
         }
         // Positions (c_res, max_res] keep their existing H3_UNUSED_DIGIT
         // value, matching the parent cell's encoding.
         (void)unused_digit;
         d_children[i] = result;
       });
     }).wait_and_throw();

    std::memcpy(children, d_children, count * sizeof(uint64_t));

    sycl::free(d_cells, q);
    sycl::free(d_children, q);
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
  return pgaccel_kernel_failure("pgaccel_h3_cell_to_center_child_bulk", &e);
} catch (...) {
  return pgaccel_kernel_failure("pgaccel_h3_cell_to_center_child_bulk", nullptr);
}

extern "C" pgaccel_status pgaccel_h3_grid_distance_bulk(const uint64_t* cells_a,
                                                        const uint64_t* cells_b, size_t count,
                                                        int32_t* distances) try {
  if (count == 0)
    return PGACCEL_OK;
  if (cells_a == nullptr || cells_b == nullptr || distances == nullptr) {
    return PGACCEL_ERROR_INIT;
  }

  try {
    sycl::queue& q = get_queue();

    uint64_t* d_a = sycl::malloc_shared<uint64_t>(count, q);
    uint64_t* d_b = sycl::malloc_shared<uint64_t>(count, q);
    int32_t* d_dist = sycl::malloc_shared<int32_t>(count, q);

    if (!d_a || !d_b || !d_dist) {
      sycl::free(d_a, q);
      sycl::free(d_b, q);
      sycl::free(d_dist, q);
      return PGACCEL_ERROR_OOM;
    }

    std::memcpy(d_a, cells_a, count * sizeof(uint64_t));
    std::memcpy(d_b, cells_b, count * sizeof(uint64_t));

    const int max_res = H3_MAX_RESOLUTION;
    const uint64_t digit_mask = H3_DIGIT_MASK;

    q.submit([&](sycl::handler& h) {
       h.parallel_for(sycl::range<1>(count), [=](sycl::id<1> id) {
         const size_t i = id[0];
         uint64_t a = d_a[i];
         uint64_t b = d_b[i];

         if (a == 0 || b == 0) {
           d_dist[i] = -1;
           return;
         }

         int res_a = static_cast<int>((a >> 52) & 0xF);
         int res_b = static_cast<int>((b >> 52) & 0xF);
         if (res_a != res_b) {
           d_dist[i] = -1;
           return;
         }

         int base_a = static_cast<int>((a >> 45) & 0x7F);
         int base_b = static_cast<int>((b >> 45) & 0x7F);
         if (base_a != base_b) {
           d_dist[i] = -1;
           return;
         }

         if (a == b) {
           d_dist[i] = 0;
           return;
         }

         // Inline cell_to_ijk for both cells
         // Direction vectors in IJK space for digits 0-6
         const int dir_i[7] = {0, 1, 0, -1, -1, 0, 1};
         const int dir_j[7] = {0, 0, 1, 1, 0, -1, -1};
         const int dir_k[7] = {0, 0, 0, 0, 1, 1, 0};

         int ia = 0, ja = 0, ka = 0;
         int ib = 0, jb = 0, kb = 0;
         for (int r = 1; r <= res_a; r++) {
           int shift = (max_res - r) * 3;
           int da = static_cast<int>((a >> shift) & digit_mask);
           int db = static_cast<int>((b >> shift) & digit_mask);
           if (da > 6) {
             ia = ja = ka = 0;
           } else {
             ia = ia * 3 + dir_i[da];
             ja = ja * 3 + dir_j[da];
             ka = ka * 3 + dir_k[da];
           }
           if (db > 6) {
             ib = jb = kb = 0;
           } else {
             ib = ib * 3 + dir_i[db];
             jb = jb * 3 + dir_j[db];
             kb = kb * 3 + dir_k[db];
           }
         }

         // IJK distance: max(|di|, |dj|, |dk|) after normalisation
         int di = ia - ib, dj = ja - jb, dk = ka - kb;
         int m = di;
         if (dj < m)
           m = dj;
         if (dk < m)
           m = dk;
         di -= m;
         dj -= m;
         dk -= m;
         int d = di;
         if (dj > d)
           d = dj;
         if (dk > d)
           d = dk;
         d_dist[i] = static_cast<int32_t>(d);
       });
     }).wait_and_throw();

    std::memcpy(distances, d_dist, count * sizeof(int32_t));

    sycl::free(d_a, q);
    sycl::free(d_b, q);
    sycl::free(d_dist, q);
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
  return pgaccel_kernel_failure("pgaccel_h3_grid_distance_bulk", &e);
} catch (...) {
  return pgaccel_kernel_failure("pgaccel_h3_grid_distance_bulk", nullptr);
}

extern "C" pgaccel_status pgaccel_h3_lat_lng_to_cell_bulk(const void* lat_array,
                                                          const void* lng_array, size_t count,
                                                          int resolution, int use_fp64,
                                                          uint64_t* cell_ids, uint8_t* valid) try {
  if (count == 0)
    return PGACCEL_OK;
  if (lat_array == nullptr || lng_array == nullptr || cell_ids == nullptr || valid == nullptr) {
    return PGACCEL_ERROR_INIT;
  }
  if (resolution < 0 || resolution > H3_MAX_RESOLUTION) {
    return PGACCEL_ERROR_UNSUPPORTED;
  }

  // `use_fp64` defines the caller-provided input type. High resolutions may
  // still promote fp32 input to the fp64 kernel, but only after reading the
  // source buffers as floats.
  const bool input_is_fp64 = use_fp64 != 0;
  const auto* lats_f64 = static_cast<const double*>(lat_array);
  const auto* lngs_f64 = static_cast<const double*>(lng_array);
  const auto* lats_f32 = static_cast<const float*>(lat_array);
  const auto* lngs_f32 = static_cast<const float*>(lng_array);

  const bool want_fp64 = input_is_fp64 || resolution >= 12;

  // Resolutions >= 12 need fp64 precision on the projection math; fp32
  // cannot represent the sub-metre grid spacing. fp64 is always available:
  // native on CUDA/ROCm/Level Zero, AdaptiveCpp soft-fp64 on Metal.

  // Trig-heavy — excellent GPU candidate. The fp32 kernel below rejects
  // res >= 12 in-kernel because precision is insufficient. The fp64 branch
  // (handled via a separate kernel dispatched when `want_fp64` is set)
  // handles the high-resolution case.
  //
  // ─── Argbuffer-reflection workaround (same slab pattern as spatial dispatch) ───
  // The natural lambda capture for these kernels is six typed device
  // pointers (lats/lngs/cells/valid) plus a few scalars.
  // On Metal the AdaptiveCpp SSCP backend, once a kernel captures
  // more than ~5 arguments, packs them into an `Input_0` argument-buffer
  // struct passed as a single `[[buffer(1)]]`. Metal's
  // `[MTLFunction newArgumentEncoderWithBufferIndex:reflection:]` then
  // refuses to index past slot 0 of that buffer in forked PG backends
  // (`bufferIndex 1 does not identify an argument buffer` assertion at
  // _MTLFunction.mm:11540, repro under cold-cache test_fork).
  //
  // The fix: stage every per-row buffer end-to-end into
  // ONE shared-memory `uint8_t*` slab AND keep total captures at ≤4
  // so the emitter stays in the per-buffer path (no `Input_0` struct,
  // no argbuffer reflection). Sub-array byte offsets are computed
  // INSIDE the kernel from a single captured `count`, not passed as
  // scalars. Captures here are exactly: { `d_slab` pointer, `count`,
  // `res` } — three arguments. The kernel rebuilds typed
  // views via `reinterpret_cast<T*>(slab + offset)` from offsets it
  // recomputes. This keeps the kernel resident on GPU (no host
  // fallback — see CLAUDE.md rules #11/#12) while side-stepping the
  // emitter's argbuffer code path entirely.
  try {
    sycl::queue& q = get_queue();

    if (want_fp64) {
      // ---- fp64 path --------------------------------------------------
      // Soft-fp64 on Metal, native fp64 on CUDA/ROCm/L0. Keep the exact
      // conversion split into two kernels: a double-precision projection
      // kernel writes FaceIJK, then an integer-only kernel assembles H3 ids.
      // This avoids one giant MetalEmitter tree that mixes soft-fp64 math
      // with the full H3 digit/base-cell state machine.
      //
      // Flat-slab layout (8-byte items first to keep cells aligned):
      //   [0                       .. +count*8)  : double   lats[count]
      //   [lat + count*8           .. +count*8)  : double   lngs[count]
      //   [lng + count*8           .. +count*8)  : uint64_t cells[count]
      //   [align(cells + count*8)  .. +count*sizeof(FaceIJK)) : FaceIJK fijk[count]
      //   [fijk + face_ijk_bytes   .. +count)    : uint8_t  valid[count]
      //
      // Kernels rebuild these offsets from `count`, keeping captures to
      // { d_slab, row_count, res } and preserving the Metal argbuffer
      // workaround described above.
      const size_t f64_bytes = count * sizeof(double);
      const size_t cells_bytes = count * sizeof(uint64_t);
      const size_t face_ijk_bytes = count * sizeof(pgaccel_h3_exact::FaceIJK);
      const size_t valid_bytes = count * sizeof(uint8_t);
      constexpr size_t face_ijk_align = alignof(pgaccel_h3_exact::FaceIJK);
      auto align_up = [](size_t value, size_t alignment) {
        return ((value + alignment - 1) / alignment) * alignment;
      };

      const size_t lat_off = 0;
      const size_t lng_off = lat_off + f64_bytes;
      const size_t cells_off = lng_off + f64_bytes;
      const size_t face_ijk_off = align_up(cells_off + cells_bytes, face_ijk_align);
      const size_t valid_off = face_ijk_off + face_ijk_bytes;
      const size_t stage_lat32_off = valid_off + valid_bytes;
      const size_t stage_lng32_off = stage_lat32_off + count * sizeof(float);
      const size_t slab_bytes = stage_lng32_off + count * sizeof(float);

      uint8_t* d_slab = sycl::malloc_shared<uint8_t>(slab_bytes, q);
      if (!d_slab) {
        return PGACCEL_ERROR_OOM;
      }

      // Stage caller bytes without interpreting them on the host. fp32 input
      // is promoted by a device kernel before the exact projection kernels.
      if (input_is_fp64) {
        std::memcpy(d_slab + lat_off, lats_f64, f64_bytes);
        std::memcpy(d_slab + lng_off, lngs_f64, f64_bytes);
      } else {
        const size_t f32_bytes = count * sizeof(float);
        std::memcpy(d_slab + stage_lat32_off, lats_f32, f32_bytes);
        std::memcpy(d_slab + stage_lng32_off, lngs_f32, f32_bytes);

        const size_t row_count = count;
        q.parallel_for(sycl::range<1>(row_count), [=](sycl::id<1> id) {
           const size_t i = id[0];
           const size_t k_f64_bytes = row_count * sizeof(double);
           const size_t k_cells_bytes = row_count * sizeof(uint64_t);
           const size_t k_face_ijk_align = alignof(pgaccel_h3_exact::FaceIJK);
           const size_t k_face_ijk_bytes = row_count * sizeof(pgaccel_h3_exact::FaceIJK);
           const size_t k_lng_off = k_f64_bytes;
           const size_t k_cells_off = k_lng_off + k_f64_bytes;
           const size_t k_face_ijk_off =
               ((k_cells_off + k_cells_bytes + k_face_ijk_align - 1) / k_face_ijk_align) *
               k_face_ijk_align;
           const size_t k_valid_off = k_face_ijk_off + k_face_ijk_bytes;
           const size_t k_stage_lat32_off = k_valid_off + row_count * sizeof(uint8_t);
           const size_t k_stage_lng32_off = k_stage_lat32_off + row_count * sizeof(float);

           auto* d_lats64 = reinterpret_cast<double*>(d_slab);
           auto* d_lngs64 = reinterpret_cast<double*>(d_slab + k_lng_off);
           const auto* d_lats32 = reinterpret_cast<const float*>(d_slab + k_stage_lat32_off);
           const auto* d_lngs32 = reinterpret_cast<const float*>(d_slab + k_stage_lng32_off);
           d_lats64[i] = static_cast<double>(d_lats32[i]);
           d_lngs64[i] = static_cast<double>(d_lngs32[i]);
         }).wait_and_throw();
      }
      // Outputs: zero-init so partial failure leaves a defined state.
      std::memset(d_slab + cells_off, 0, cells_bytes);
      std::memset(d_slab + face_ijk_off, 0, face_ijk_bytes);
      std::memset(d_slab + valid_off, 0, valid_bytes);

      const int res = resolution;
      const size_t row_count = count;

      q.submit([&](sycl::handler& h) {
         // Captures: { d_slab, row_count, res } — 3 args.
         // Below the AdaptiveCpp Metal-SSCP `Input_0` packing threshold,
         // so the emitter keeps each as a separate `[[buffer(N)]]` and
         // no argument-buffer reflection is invoked at dispatch time.
         h.parallel_for(sycl::range<1>(count), [=](sycl::id<1> id) {
           const size_t i = id[0];
           // Recompute offsets in-kernel from row_count. Layout MUST
           // match the host-side computation above byte-for-byte.
           const size_t k_f64_bytes = row_count * sizeof(double);
           const size_t k_cells_bytes = row_count * sizeof(uint64_t);
           const size_t k_face_ijk_align = alignof(pgaccel_h3_exact::FaceIJK);
           const size_t k_face_ijk_bytes = row_count * sizeof(pgaccel_h3_exact::FaceIJK);
           const size_t k_lat_off = 0;
           const size_t k_lng_off = k_lat_off + k_f64_bytes;
           const size_t k_cells_off = k_lng_off + k_f64_bytes;
           const size_t k_face_ijk_off =
               ((k_cells_off + k_cells_bytes + k_face_ijk_align - 1) / k_face_ijk_align) *
               k_face_ijk_align;
           const size_t k_valid_off = k_face_ijk_off + k_face_ijk_bytes;

           const double* d_lats = reinterpret_cast<const double*>(d_slab + k_lat_off);
           const double* d_lngs = reinterpret_cast<const double*>(d_slab + k_lng_off);
           auto* d_fijk = reinterpret_cast<pgaccel_h3_exact::FaceIJK*>(d_slab + k_face_ijk_off);
           uint8_t* d_valid = d_slab + k_valid_off;

           double lat_deg = d_lats[i];
           double lng_deg = d_lngs[i];

           pgaccel_h3_exact::FaceIJK projected;
           d_valid[i] = h3_exact_project_face_ijk(lat_deg, lng_deg, res, projected);
           d_fijk[i] = projected;
         });
       }).wait_and_throw();

      q.submit([&](sycl::handler& h) {
         h.parallel_for(sycl::range<1>(count), [=](sycl::id<1> id) {
           const size_t i = id[0];
           const size_t k_f64_bytes = row_count * sizeof(double);
           const size_t k_cells_bytes = row_count * sizeof(uint64_t);
           const size_t k_face_ijk_align = alignof(pgaccel_h3_exact::FaceIJK);
           const size_t k_face_ijk_bytes = row_count * sizeof(pgaccel_h3_exact::FaceIJK);
           const size_t k_lng_off = k_f64_bytes;
           const size_t k_cells_off = k_lng_off + k_f64_bytes;
           const size_t k_face_ijk_off =
               ((k_cells_off + k_cells_bytes + k_face_ijk_align - 1) / k_face_ijk_align) *
               k_face_ijk_align;
           const size_t k_valid_off = k_face_ijk_off + k_face_ijk_bytes;

           uint64_t* d_cells = reinterpret_cast<uint64_t*>(d_slab + k_cells_off);
           const auto* d_fijk =
               reinterpret_cast<const pgaccel_h3_exact::FaceIJK*>(d_slab + k_face_ijk_off);
           uint8_t* d_valid = d_slab + k_valid_off;

           if (d_valid[i] == 0) {
             d_cells[i] = 0;
             return;
           }

           uint64_t cell = 0;
           uint8_t valid_cell = 0;
           h3_exact_finalize_face_ijk(d_fijk[i], res, cell, valid_cell);
           d_cells[i] = cell;
           d_valid[i] = valid_cell;
         });
       }).wait_and_throw();

      q.memcpy(cell_ids, d_slab + cells_off, cells_bytes).wait_and_throw();
      q.memcpy(valid, d_slab + valid_off, valid_bytes).wait_and_throw();

      sycl::free(d_slab, q);
      pgaccel_record_gpu_exec();
      return PGACCEL_OK;
    }
    // ---- fp32 path (res < 12, caller did not request fp64) -------------
    //
    // Stage fp32 coordinates plus promoted f64 coordinates into the common H3
    // slab. The first kernel performs the fast fp32 conversion and marks
    // boundary-risk rows with valid=2. A split exact projection/finalization
    // pair then fixes those rows on the GPU, preserving the "exact H3 belongs
    // on device" invariant without rebuilding the monolithic soft-fp64 kernel.
    const H3LatLngCellSlabLayout layout = h3_lat_lng_cell_slab_layout(count);

    uint8_t* d_fast_slab = sycl::malloc_shared<uint8_t>(layout.slab_bytes, q);
    if (!d_fast_slab) {
      return PGACCEL_ERROR_OOM;
    }

    auto* slab_lats32 = reinterpret_cast<float*>(d_fast_slab + layout.lat32_off);
    auto* slab_lngs32 = reinterpret_cast<float*>(d_fast_slab + layout.lng32_off);
    const size_t f32_bytes = count * sizeof(float);
    std::memcpy(slab_lats32, lats_f32, f32_bytes);
    std::memcpy(slab_lngs32, lngs_f32, f32_bytes);

    const size_t row_count = count;
    q.parallel_for(sycl::range<1>(row_count), [=](sycl::id<1> id) {
       const size_t i = id[0];
       const H3LatLngCellSlabLayout k_layout = h3_lat_lng_cell_slab_layout(row_count);
       auto* d_lats64 = reinterpret_cast<double*>(d_fast_slab + k_layout.lat64_off);
       auto* d_lngs64 = reinterpret_cast<double*>(d_fast_slab + k_layout.lng64_off);
       const auto* d_lats32 = reinterpret_cast<const float*>(d_fast_slab + k_layout.lat32_off);
       const auto* d_lngs32 = reinterpret_cast<const float*>(d_fast_slab + k_layout.lng32_off);
       d_lats64[i] = static_cast<double>(d_lats32[i]);
       d_lngs64[i] = static_cast<double>(d_lngs32[i]);
     }).wait_and_throw();
    h3_zero_lat_lng_cell_slab(d_fast_slab, count);

    h3_run_fast_f32_to_common_slab(q, d_fast_slab, count, resolution);
    h3_run_exact_split_to_common_slab(q, d_fast_slab, count, resolution, /*fix_all=*/false);

    q.memcpy(cell_ids, d_fast_slab + layout.out_off, count * sizeof(uint64_t)).wait_and_throw();
    q.memcpy(valid, d_fast_slab + layout.valid_off, count * sizeof(uint8_t)).wait_and_throw();

    sycl::free(d_fast_slab, q);
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
  return pgaccel_kernel_failure("pgaccel_h3_lat_lng_to_cell_bulk", &e);
} catch (...) {
  return pgaccel_kernel_failure("pgaccel_h3_lat_lng_to_cell_bulk", nullptr);
}

static pgaccel_status
h3_lat_lng_count_bulk_device_direct(const double* lat_array, const double* lng_array,
                                    const float* lat_f32_array, const float* lng_f32_array,
                                    size_t count, int resolution, pgaccel_agg_state** out_state) {
  if (out_state == nullptr)
    return PGACCEL_ERROR_INIT;
  *out_state = nullptr;

  if (lat_array == nullptr || lng_array == nullptr)
    return PGACCEL_ERROR_INIT;
  if ((lat_f32_array == nullptr) != (lng_f32_array == nullptr))
    return PGACCEL_ERROR_INIT;

  try {
    sycl::queue& q = get_queue();

    const H3LatLngCellSlabLayout layout = h3_lat_lng_cell_slab_layout(count);
    uint8_t* d_slab = sycl::malloc_shared<uint8_t>(layout.slab_bytes, q);
    if (d_slab == nullptr)
      return PGACCEL_ERROR_OOM;
    H3UsmAllocationGuard slab_guard(q, d_slab);

    auto* slab_lats32 = reinterpret_cast<float*>(d_slab + layout.lat32_off);
    auto* slab_lngs32 = reinterpret_cast<float*>(d_slab + layout.lng32_off);
    const size_t f64_bytes = count * sizeof(double);
    std::memcpy(d_slab + layout.lat64_off, lat_array, f64_bytes);
    std::memcpy(d_slab + layout.lng64_off, lng_array, f64_bytes);
    if (resolution < 8) {
      if (lat_f32_array != nullptr && lng_f32_array != nullptr) {
        const size_t f32_bytes = count * sizeof(float);
        std::memcpy(slab_lats32, lat_f32_array, f32_bytes);
        std::memcpy(slab_lngs32, lng_f32_array, f32_bytes);
      } else {
        const size_t row_count = count;
        q.parallel_for(sycl::range<1>(row_count), [=](sycl::id<1> id) {
           const size_t i = id[0];
           const H3LatLngCellSlabLayout k_layout = h3_lat_lng_cell_slab_layout(row_count);
           const auto* d_lats64 =
               reinterpret_cast<const double*>(d_slab + k_layout.lat64_off);
           const auto* d_lngs64 =
               reinterpret_cast<const double*>(d_slab + k_layout.lng64_off);
           auto* d_lats32 = reinterpret_cast<float*>(d_slab + k_layout.lat32_off);
           auto* d_lngs32 = reinterpret_cast<float*>(d_slab + k_layout.lng32_off);
           d_lats32[i] = static_cast<float>(d_lats64[i]);
           d_lngs32[i] = static_cast<float>(d_lngs64[i]);
         }).wait_and_throw();
      }
    }
    h3_zero_lat_lng_cell_slab(d_slab, count);

    if (resolution >= 8) {
      h3_run_exact_split_to_common_slab(q, d_slab, count, resolution, /*fix_all=*/true);
    } else {
      h3_run_fast_f32_to_common_slab(q, d_slab, count, resolution);
      h3_run_exact_split_to_common_slab(q, d_slab, count, resolution, /*fix_all=*/false);
    }
    h3_validate_common_slab_keys(q, d_slab, count);
    pgaccel_record_gpu_exec();

    const auto* invalid = reinterpret_cast<const uint32_t*>(d_slab + layout.invalid_off);
    if (*invalid != 0) {
      slab_guard.free_now();
      return PGACCEL_ERROR;
    }

    auto* mutable_keys = reinterpret_cast<int64_t*>(d_slab + layout.out_off);
    const size_t max_distinct = h3_cell_count_at_resolution(resolution);
    pgaccel_agg_state* state = nullptr;
    const pgaccel_status count_status =
        pgaccel_hash_count_i64_device_hash_execute_bounded_checked(
            mutable_keys, count, max_distinct, &state);
    if (count_status != PGACCEL_OK) {
      slab_guard.free_now();
      return count_status;
    }

    *out_state = state;
    slab_guard.free_now();
    return PGACCEL_OK;
  } catch (const pgaccel_no_device_error&) {
    return PGACCEL_ERROR_NO_DEVICE;
  } catch (const std::bad_alloc&) {
    return PGACCEL_ERROR_OOM;
  } catch (const std::exception& e) {
    std::fprintf(stderr, "pgaccel: h3_lat_lng_count_bulk direct GPU path failed: %s\n", e.what());
    return PGACCEL_ERROR;
  } catch (...) {
    std::fprintf(stderr, "pgaccel: h3_lat_lng_count_bulk direct GPU path failed (unknown)\n");
    return PGACCEL_ERROR;
  }
}

extern "C" pgaccel_status pgaccel_h3_lat_lng_count_bulk(const double* lat_array,
                                                        const double* lng_array, size_t count,
                                                        int resolution,
                                                        pgaccel_agg_state** out_state) try {
  if (out_state == nullptr)
    return PGACCEL_ERROR_INIT;
  *out_state = nullptr;
  if (count == 0)
    return PGACCEL_OK;
  if (lat_array == nullptr || lng_array == nullptr)
    return PGACCEL_ERROR_INIT;
  if (resolution < 0 || resolution > H3_MAX_RESOLUTION)
    return PGACCEL_ERROR_UNSUPPORTED;

  return h3_lat_lng_count_bulk_device_direct(lat_array, lng_array, nullptr, nullptr, count,
                                             resolution, out_state);
} catch (const pgaccel_no_device_error&) {
  return PGACCEL_ERROR_NO_DEVICE;
} catch (const std::exception& e) {
  return pgaccel_kernel_failure("pgaccel_h3_lat_lng_count_bulk", &e);
} catch (...) {
  return pgaccel_kernel_failure("pgaccel_h3_lat_lng_count_bulk", nullptr);
}

extern "C" pgaccel_status
pgaccel_h3_lat_lng_count_bulk_f32_exact(const float* lat_f32_array, const float* lng_f32_array,
                                        const double* lat_exact_array,
                                        const double* lng_exact_array, size_t count, int resolution,
                                        pgaccel_agg_state** out_state) try {
  if (out_state == nullptr)
    return PGACCEL_ERROR_INIT;
  *out_state = nullptr;
  if (count == 0)
    return PGACCEL_OK;
  if (lat_f32_array == nullptr || lng_f32_array == nullptr || lat_exact_array == nullptr ||
      lng_exact_array == nullptr) {
    return PGACCEL_ERROR_INIT;
  }
  if (resolution < 0 || resolution > H3_MAX_RESOLUTION)
    return PGACCEL_ERROR_UNSUPPORTED;
  if (resolution >= 8)
    return pgaccel_h3_lat_lng_count_bulk(lat_exact_array, lng_exact_array, count, resolution,
                                         out_state);

  return h3_lat_lng_count_bulk_device_direct(lat_exact_array, lng_exact_array, lat_f32_array,
                                             lng_f32_array, count, resolution, out_state);
} catch (const pgaccel_no_device_error&) {
  return PGACCEL_ERROR_NO_DEVICE;
} catch (const std::exception& e) {
  return pgaccel_kernel_failure("pgaccel_h3_lat_lng_count_bulk_f32_exact", &e);
} catch (...) {
  return pgaccel_kernel_failure("pgaccel_h3_lat_lng_count_bulk_f32_exact", nullptr);
}

extern "C" pgaccel_status pgaccel_h3_lat_lng_count_resident_bulk(
    const double* lat_exact_array, const double* lng_exact_array, const float* lat_f32_array,
    const float* lng_f32_array, size_t count, int resolution, pgaccel_agg_state** out_state) try {
  if (out_state == nullptr)
    return PGACCEL_ERROR_INIT;
  *out_state = nullptr;
  if (count == 0)
    return PGACCEL_OK;
  if (lat_exact_array == nullptr || lng_exact_array == nullptr || lat_f32_array == nullptr ||
      lng_f32_array == nullptr)
    return PGACCEL_ERROR_INIT;
  if (resolution < 0 || resolution > H3_MAX_RESOLUTION)
    return PGACCEL_ERROR_UNSUPPORTED;

  try {
    sycl::queue& q = get_queue();

    const H3LatLngCellSlabLayout layout = h3_lat_lng_cell_slab_layout(count);
    uint8_t* d_slab = sycl::malloc_shared<uint8_t>(layout.slab_bytes, q);
    if (d_slab == nullptr)
      return PGACCEL_ERROR_OOM;
    H3UsmAllocationGuard slab_guard(q, d_slab);

    h3_zero_lat_lng_cell_slab(d_slab, count);

    if (resolution >= 8) {
      h3_run_exact_split_device_to_common_slab(q, d_slab, lat_exact_array, lng_exact_array, count,
                                               resolution, /*fix_all=*/true);
    } else {
      h3_run_fast_f32_device_to_common_slab(q, d_slab, lat_f32_array, lng_f32_array, count,
                                            resolution);
      h3_run_exact_split_device_to_common_slab(q, d_slab, lat_exact_array, lng_exact_array, count,
                                               resolution, /*fix_all=*/false);
    }
    h3_validate_common_slab_keys(q, d_slab, count);
    pgaccel_record_gpu_exec();

    auto* keys = reinterpret_cast<int64_t*>(d_slab + layout.out_off);
    auto* invalid = reinterpret_cast<uint32_t*>(d_slab + layout.invalid_off);
    if (*invalid != 0) {
      slab_guard.free_now();
      return PGACCEL_ERROR;
    }

    const size_t max_distinct = h3_cell_count_at_resolution(resolution);
    pgaccel_agg_state* state = nullptr;
    const pgaccel_status count_status =
        pgaccel_hash_count_i64_device_hash_execute_bounded_checked(keys, count, max_distinct,
                                                                   &state);
    if (count_status != PGACCEL_OK) {
      slab_guard.free_now();
      return count_status;
    }

    *out_state = state;
    slab_guard.free_now();
    return PGACCEL_OK;
  } catch (const pgaccel_no_device_error&) {
    return PGACCEL_ERROR_NO_DEVICE;
  } catch (const std::bad_alloc&) {
    return PGACCEL_ERROR_OOM;
  } catch (const std::exception& e) {
    std::fprintf(stderr, "pgaccel: h3_lat_lng_count_resident_bulk failed: %s\n", e.what());
    return PGACCEL_ERROR;
  } catch (...) {
    std::fprintf(stderr, "pgaccel: h3_lat_lng_count_resident_bulk failed (unknown)\n");
    return PGACCEL_ERROR;
  }
} catch (const pgaccel_no_device_error&) {
  return PGACCEL_ERROR_NO_DEVICE;
} catch (const std::exception& e) {
  return pgaccel_kernel_failure("pgaccel_h3_lat_lng_count_resident_bulk", &e);
} catch (...) {
  return pgaccel_kernel_failure("pgaccel_h3_lat_lng_count_resident_bulk", nullptr);
}

// ---------------------------------------------------------------------------
// Variable-output H3 kernels (two-pass output-size + emit protocol)
// ---------------------------------------------------------------------------
//
// All kernels in this section follow the contract published in
// pgaccel_ffi.h §"H3 Variable-Output Kernels":
//
//   1. *_output_size — fills out_offsets[count+1] with cumulative output
//      counts; out_offsets[0] = 0; out_offsets[count] = total elements.
//   2. *_emit        — writes each input's outputs into
//      out_buf[out_offsets[i] .. out_offsets[i+1]] using the offsets buffer
//      computed in pass 1.
//
// A variable-output ABI may return PGACCEL_ERROR_UNSUPPORTED before either
// pass writes output. This is the fail-closed contract for operations that do
// not yet have exact device semantics. Callers must keep those operations out
// of production planner registration and leave the query on h3-pg's native
// implementation.
// ---------------------------------------------------------------------------

// ---------------------------------------------------------------------------
// h3_grid_disk
// ---------------------------------------------------------------------------

extern "C" pgaccel_status pgaccel_h3_grid_disk_output_size(const uint64_t* cells, size_t count,
                                                           int32_t k, uint32_t* out_offsets) {
  if (count == 0) {
    if (out_offsets != nullptr)
      out_offsets[0] = 0;
    return PGACCEL_OK;
  }
  if (cells == nullptr || out_offsets == nullptr)
    return PGACCEL_ERROR_INIT;
  (void)k;
  return PGACCEL_ERROR_UNSUPPORTED;
}

extern "C" pgaccel_status pgaccel_h3_grid_disk_emit(const uint64_t* cells, size_t count, int32_t k,
                                                    const uint32_t* offsets, uint64_t* out_cells) {
  if (count == 0)
    return PGACCEL_OK;
  if (cells == nullptr || offsets == nullptr || out_cells == nullptr)
    return PGACCEL_ERROR_INIT;
  (void)k;
  return PGACCEL_ERROR_UNSUPPORTED;
}

// ---------------------------------------------------------------------------
// h3_grid_ring_unsafe
// ---------------------------------------------------------------------------

extern "C" pgaccel_status pgaccel_h3_grid_ring_unsafe_output_size(const uint64_t* cells,
                                                                  size_t count, int32_t k,
                                                                  uint32_t* out_offsets) {
  if (count == 0) {
    if (out_offsets != nullptr)
      out_offsets[0] = 0;
    return PGACCEL_OK;
  }
  if (cells == nullptr || out_offsets == nullptr)
    return PGACCEL_ERROR_INIT;
  (void)k;
  return PGACCEL_ERROR_UNSUPPORTED;
}

extern "C" pgaccel_status pgaccel_h3_grid_ring_unsafe_emit(const uint64_t* cells, size_t count,
                                                           int32_t k, const uint32_t* offsets,
                                                           uint64_t* out_cells) {
  if (count == 0)
    return PGACCEL_OK;
  if (cells == nullptr || offsets == nullptr || out_cells == nullptr)
    return PGACCEL_ERROR_INIT;
  (void)k;
  return PGACCEL_ERROR_UNSUPPORTED;
}

// ---------------------------------------------------------------------------
// h3_cell_to_children
// ---------------------------------------------------------------------------
//
// For an input cell at resolution `cr` and target `child_res`:
//   hexagon child count = 7 ^ (child_res - cr)
//   pentagon child count = 5 * 7 ^ (child_res - cr - 1)  if child_res > cr
//   if child_res == cr → 1 (the cell itself)
// We emit children in canonical lexicographic order over the new digit
// slots [cr+1 .. child_res]: (0,0,...,0), (0,0,...,1), ..., (6,6,...,6).
// A child digit of 0 in the leading new position produces the centre child;
// other combinations produce off-centre children. For pentagon parents we
// skip combinations whose leading new digit is 1 (the missing direction).
// ---------------------------------------------------------------------------

struct H3ChildrenSizeCompletion {
  int32_t status;
  int32_t detail;
  uint64_t row_count;
  uint64_t output_count;
};

class H3ChildrenSizeCompletionKernel;

extern "C" pgaccel_status pgaccel_h3_cell_to_children_output_size(const uint64_t* cells,
                                                                  size_t count, int32_t child_res,
                                                                  uint32_t* out_offsets) try {
  if (count == 0) {
    if (out_offsets != nullptr)
      out_offsets[0] = 0;
    return PGACCEL_OK;
  }
  if (cells == nullptr || out_offsets == nullptr)
    return PGACCEL_ERROR_INIT;
  if (child_res < 0 || child_res > H3_MAX_RESOLUTION)
    return PGACCEL_ERROR_UNSUPPORTED;

  try {
    sycl::queue& q = get_queue();

    // Keep Metal SSCP captures below the argbuffer-reflection threshold:
    // one shared slab plus {count, child_res}. The kernel recomputes typed
    // subviews from count, mirroring the lat/lng slab pattern above.
    const size_t cells_bytes = count * sizeof(uint64_t);
    const size_t counts_off = cells_bytes;
    const size_t counts_bytes = count * sizeof(uint32_t);
    const size_t offsets_off = counts_off + counts_bytes;
    const size_t offsets_bytes = (count + 1) * sizeof(uint32_t);
    const size_t completion_off = h3_align_up_size(offsets_off + offsets_bytes,
                                                   alignof(H3ChildrenSizeCompletion));
    const size_t slab_bytes = completion_off + sizeof(H3ChildrenSizeCompletion);

    uint8_t* d_slab = sycl::malloc_shared<uint8_t>(slab_bytes, q);
    if (!d_slab) {
      return PGACCEL_ERROR_OOM;
    }

    std::memcpy(d_slab, cells, cells_bytes);

    const int32_t cr = child_res;
    const size_t row_count = count;

    q.submit([&](sycl::handler& h) {
       h.parallel_for(sycl::range<1>(row_count), [=](sycl::id<1> id) {
         const size_t k_cells_off = 0;
         const size_t k_counts_off = row_count * sizeof(uint64_t);
         const uint64_t* d_cells = reinterpret_cast<const uint64_t*>(d_slab + k_cells_off);
         uint32_t* d_counts = reinterpret_cast<uint32_t*>(d_slab + k_counts_off);

         const uint64_t pent_low = (1ULL << 4) | (1ULL << 14) | (1ULL << 24) | (1ULL << 33) |
                                   (1ULL << 49) | (1ULL << 50) | (1ULL << 58) | (1ULL << 63);
         const uint64_t pent_high = (1ULL << (72 - 64)) | (1ULL << (83 - 64)) |
                                    (1ULL << (97 - 64)) | (1ULL << (107 - 64)) |
                                    (1ULL << (117 - 64));
         const int max_res = 15;

         const size_t i = id[0];
         const uint64_t cell = d_cells[i];
         if (cell == 0) {
           d_counts[i] = 0;
           return;
         }
         const int res = static_cast<int>((cell >> 52) & 0xF);
         if (cr < res) {
           d_counts[i] = 0;
           return;
         }
         if (cr == res) {
           d_counts[i] = 1;
           return;
         }
         const int delta = cr - res;

         const int base = static_cast<int>((cell >> 45) & 0x7F);
         const bool is_pent_base = (base < 64) ? ((pent_low >> base) & 1ULL) != 0
                                               : ((pent_high >> (base - 64)) & 1ULL) != 0;
         bool pent = is_pent_base;
         if (pent) {
           for (int r = 1; r <= res; r++) {
             const int shift = (max_res - r) * 3;
             if (((cell >> shift) & 0x7) != 0) {
               pent = false;
               break;
             }
           }
         }

         // 7^delta hexagon children, 5*7^(delta-1) pentagon children
         uint32_t total = 1;
         for (int d = 0; d < delta; d++)
           total *= 7;
         if (pent && delta >= 1) {
           total = (total / 7) * 5;
         }
         d_counts[i] = total;
       });
     }).wait_and_throw();

    q.single_task<H3ChildrenSizeCompletionKernel>([=]() {
       const size_t k_counts_off = row_count * sizeof(uint64_t);
       const size_t k_offsets_off = k_counts_off + row_count * sizeof(uint32_t);
       const size_t k_offsets_bytes = (row_count + 1) * sizeof(uint32_t);
       const size_t k_completion_off =
           h3_align_up_size(k_offsets_off + k_offsets_bytes, alignof(H3ChildrenSizeCompletion));
       const auto* d_counts = reinterpret_cast<const uint32_t*>(d_slab + k_counts_off);
       auto* d_offsets = reinterpret_cast<uint32_t*>(d_slab + k_offsets_off);
       auto* d_completion =
           reinterpret_cast<H3ChildrenSizeCompletion*>(d_slab + k_completion_off);

       d_completion->status = static_cast<int32_t>(PGACCEL_OK);
       d_completion->detail = 0;
       d_completion->row_count = row_count;
       d_completion->output_count = 0;
       d_offsets[0] = 0;

       uint64_t total = 0;
       for (size_t i = 0; i < row_count; ++i) {
         total += d_counts[i];
         if (total > UINT32_MAX) {
           d_completion->status = static_cast<int32_t>(PGACCEL_ERROR_UNSUPPORTED);
           return;
         }
         d_offsets[i + 1] = static_cast<uint32_t>(total);
       }
       d_completion->output_count = total;
     }).wait_and_throw();

    H3ChildrenSizeCompletion completion{};
    q.memcpy(&completion, d_slab + completion_off, sizeof(completion)).wait_and_throw();
    if (completion.status == static_cast<int32_t>(PGACCEL_ERROR_UNSUPPORTED)) {
      sycl::free(d_slab, q);
      return PGACCEL_ERROR_UNSUPPORTED;
    }
    if (completion.status != static_cast<int32_t>(PGACCEL_OK)) {
      sycl::free(d_slab, q);
      return PGACCEL_ERROR;
    }

    q.memcpy(out_offsets, d_slab + offsets_off, offsets_bytes).wait_and_throw();

    sycl::free(d_slab, q);
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
  return pgaccel_kernel_failure("pgaccel_h3_cell_to_children_output_size", &e);
} catch (...) {
  return pgaccel_kernel_failure("pgaccel_h3_cell_to_children_output_size", nullptr);
}

extern "C" pgaccel_status pgaccel_h3_cell_to_children_emit(const uint64_t* cells, size_t count,
                                                           int32_t child_res,
                                                           const uint32_t* offsets,
                                                           uint64_t* out_children) try {
  if (count == 0)
    return PGACCEL_OK;
  if (cells == nullptr || offsets == nullptr || out_children == nullptr)
    return PGACCEL_ERROR_INIT;
  if (child_res < 0 || child_res > H3_MAX_RESOLUTION)
    return PGACCEL_ERROR_UNSUPPORTED;

  const size_t output_count = offsets[count];
  if (output_count == 0)
    return PGACCEL_OK;

  try {
    sycl::queue& q = get_queue();

    // Same Metal SSCP capture workaround as the size pass: one slab pointer,
    // count, and child_res. Output starts on an 8-byte boundary.
    const size_t cells_bytes = count * sizeof(uint64_t);
    const size_t offsets_off = cells_bytes;
    const size_t offsets_bytes = (count + 1) * sizeof(uint32_t);
    const size_t out_off = (offsets_off + offsets_bytes + 7) & ~size_t{7};
    const size_t out_bytes = output_count * sizeof(uint64_t);
    const size_t slab_bytes = out_off + out_bytes;

    uint8_t* d_slab = sycl::malloc_shared<uint8_t>(slab_bytes, q);
    if (!d_slab) {
      return PGACCEL_ERROR_OOM;
    }

    std::memcpy(d_slab, cells, cells_bytes);
    std::memcpy(d_slab + offsets_off, offsets, offsets_bytes);
    std::memset(d_slab + out_off, 0, out_bytes);

    const int32_t cr = child_res;
    const size_t row_count = count;

    q.submit([&](sycl::handler& h) {
       h.parallel_for(sycl::range<1>(row_count), [=](sycl::id<1> id) {
         const size_t k_cells_off = 0;
         const size_t k_offsets_off = row_count * sizeof(uint64_t);
         const size_t k_offsets_bytes = (row_count + 1) * sizeof(uint32_t);
         const size_t k_out_off = (k_offsets_off + k_offsets_bytes + 7) & ~size_t{7};
         const uint64_t* d_cells = reinterpret_cast<const uint64_t*>(d_slab + k_cells_off);
         const uint32_t* d_offsets = reinterpret_cast<const uint32_t*>(d_slab + k_offsets_off);
         uint64_t* d_out = reinterpret_cast<uint64_t*>(d_slab + k_out_off);

         const int max_res = 15;
         const uint64_t res_mask = 0xFULL << 52;
         const uint64_t pent_low = (1ULL << 4) | (1ULL << 14) | (1ULL << 24) | (1ULL << 33) |
                                   (1ULL << 49) | (1ULL << 50) | (1ULL << 58) | (1ULL << 63);
         const uint64_t pent_high = (1ULL << (72 - 64)) | (1ULL << (83 - 64)) |
                                    (1ULL << (97 - 64)) | (1ULL << (107 - 64)) |
                                    (1ULL << (117 - 64));

         const size_t i = id[0];
         const uint64_t cell = d_cells[i];
         const uint32_t start = d_offsets[i];
         const uint32_t end = d_offsets[i + 1];
         const uint32_t want = end - start;
         if (want == 0)
           return;
         if (cell == 0)
           return;

         const int res = static_cast<int>((cell >> 52) & 0xF);
         if (cr == res) {
           d_out[start] = cell;
           return;
         }
         if (cr < res) {
           // size pass should have given want = 0; defensive no-op
           return;
         }

         // Pentagon detection
         const int base = static_cast<int>((cell >> 45) & 0x7F);
         const bool is_pent_base = (base < 64) ? ((pent_low >> base) & 1ULL) != 0
                                               : ((pent_high >> (base - 64)) & 1ULL) != 0;
         bool pent = is_pent_base;
         if (pent) {
           for (int r = 1; r <= res; r++) {
             const int shift = (max_res - r) * 3;
             if (((cell >> shift) & 0x7) != 0) {
               pent = false;
               break;
             }
           }
         }

         const int delta = cr - res;
         // For each child index 0..want-1, decode it as base-7 digit
         // sequence over the `delta` new digit slots. For pentagons, the
         // leading new digit is encoded base-6 (skipping digit 1).
         uint32_t out_idx = 0;
         // Precompute leading-digit divisor: number of children below leading
         // 7 ^ (delta - 1) for hex; same for pentagon (because remaining
         // digits are still base-7).
         uint32_t below_lead = 1;
         for (int d = 0; d < delta - 1; d++)
           below_lead *= 7;

         while (out_idx < want) {
           // Build the new resolution + digit sequence
           uint64_t result = (cell & ~res_mask) | (static_cast<uint64_t>(cr) << 52);

           // Decode child index into per-slot digits
           uint32_t idx = out_idx;
           // Leading digit selection
           int lead_choice;
           uint32_t lead_div;
           if (pent && delta >= 1) {
             // 5 leading choices: digits {0, 2, 3, 4, 5, 6} (skip 1)
             lead_div = below_lead;
             uint32_t lead_idx = idx / lead_div;
             // Map lead_idx 0..4 -> digit values (skip pentagon's missing
             // direction = digit 1).
             const int lead_map[5] = {0, 2, 3, 4, 5};
             lead_choice = lead_map[lead_idx > 4 ? 4 : lead_idx];
             idx = idx % lead_div;
           } else {
             lead_div = below_lead;
             uint32_t lead_idx = idx / lead_div;
             lead_choice = static_cast<int>(lead_idx);
             idx = idx % lead_div;
           }

           // Set leading new digit at resolution res+1
           {
             const int slot = res + 1;
             const int shift = (max_res - slot) * 3;
             const uint64_t mask = uint64_t{0x7} << shift;
             result = (result & ~mask) | (static_cast<uint64_t>(lead_choice & 0x7) << shift);
           }

           // Remaining new digits at resolutions res+2 .. cr; base-7
           for (int slot = res + 2; slot <= cr; slot++) {
             // Compute divisor for this slot
             uint32_t div = 1;
             for (int d = 0; d < (cr - slot); d++)
               div *= 7;
             uint32_t digit_idx = (div == 0) ? 0 : (idx / div);
             idx = (div == 0) ? idx : (idx % div);
             const int shift = (max_res - slot) * 3;
             const uint64_t mask = uint64_t{0x7} << shift;
             result = (result & ~mask) |
                      (static_cast<uint64_t>(static_cast<int>(digit_idx) & 0x7) << shift);
           }

           // Slots cr+1 .. max_res must be unused (7).
           for (int slot = cr + 1; slot <= max_res; slot++) {
             const int shift = (max_res - slot) * 3;
             const uint64_t mask = uint64_t{0x7} << shift;
             result = (result & ~mask) | (uint64_t{7} << shift);
           }

           d_out[start + out_idx] = result;
           out_idx++;
         }
       });
     }).wait_and_throw();

    q.memcpy(out_children, d_slab + out_off, out_bytes).wait_and_throw();

    sycl::free(d_slab, q);
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
  return pgaccel_kernel_failure("pgaccel_h3_cell_to_children_emit", &e);
} catch (...) {
  return pgaccel_kernel_failure("pgaccel_h3_cell_to_children_emit", nullptr);
}

// ---------------------------------------------------------------------------
// h3_cell_to_boundary
// ---------------------------------------------------------------------------
//
// Exact boundary generation requires H3's icosahedral edge corrections. The
// former implementation produced an approximate host-side polygon and
// counted it as GPU work. Until exact device semantics land, both passes are
// deliberately unavailable for nonempty input.
// ---------------------------------------------------------------------------

extern "C" pgaccel_status pgaccel_h3_cell_to_boundary_output_size(const uint64_t* cells,
                                                                  size_t count,
                                                                  uint32_t* out_offsets) {
  if (count == 0) {
    if (out_offsets != nullptr)
      out_offsets[0] = 0;
    return PGACCEL_OK;
  }
  if (cells == nullptr || out_offsets == nullptr)
    return PGACCEL_ERROR_INIT;
  return PGACCEL_ERROR_UNSUPPORTED;
}

extern "C" pgaccel_status pgaccel_h3_cell_to_boundary_emit(const uint64_t* cells, size_t count,
                                                           const uint32_t* offsets,
                                                           double* out_coords) {
  if (count == 0)
    return PGACCEL_OK;
  if (cells == nullptr || offsets == nullptr || out_coords == nullptr)
    return PGACCEL_ERROR_INIT;
  return PGACCEL_ERROR_UNSUPPORTED;
}

// ---------------------------------------------------------------------------
// h3_polyfill
// ---------------------------------------------------------------------------
//
// Exact polygon-to-cells needs H3's containment and topology semantics. The
// former bbox sampler was host-computed and only approximate, so nonempty
// calls now fail closed until an exact device implementation is available.
// ---------------------------------------------------------------------------

extern "C" pgaccel_status pgaccel_h3_polyfill_output_size(const float* coords,
                                                          const uint32_t* ring_offsets,
                                                          size_t ring_count, int32_t resolution,
                                                          uint32_t* out_offsets) {
  if (ring_count == 0) {
    if (out_offsets != nullptr)
      out_offsets[0] = 0;
    return PGACCEL_OK;
  }
  if (coords == nullptr || ring_offsets == nullptr || out_offsets == nullptr)
    return PGACCEL_ERROR_INIT;
  (void)resolution;
  return PGACCEL_ERROR_UNSUPPORTED;
}

extern "C" pgaccel_status pgaccel_h3_polyfill_emit(const float* coords,
                                                   const uint32_t* ring_offsets, size_t ring_count,
                                                   int32_t resolution, const uint32_t* offsets,
                                                   uint64_t* out_cells) {
  if (ring_count == 0)
    return PGACCEL_OK;
  if (coords == nullptr || ring_offsets == nullptr || offsets == nullptr || out_cells == nullptr)
    return PGACCEL_ERROR_INIT;
  (void)resolution;
  return PGACCEL_ERROR_UNSUPPORTED;
}

// ---------------------------------------------------------------------------
// h3_cells_to_multi_polygon
// ---------------------------------------------------------------------------
//
// Exact multi-polygon output requires shared-edge cancellation and faithful
// ring linking. Emitting one approximate ring per input cell is not equivalent
// to h3-pg, so nonempty calls fail closed until exact device topology lands.
// ---------------------------------------------------------------------------

extern "C" pgaccel_status pgaccel_h3_cells_to_multi_polygon_output_size(const uint64_t* cells,
                                                                        size_t count,
                                                                        uint32_t* out_ring_offsets,
                                                                        uint32_t* out_ring_count) {
  if (count == 0) {
    if (out_ring_count != nullptr)
      *out_ring_count = 0;
    if (out_ring_offsets != nullptr)
      out_ring_offsets[0] = 0;
    return PGACCEL_OK;
  }
  if (cells == nullptr || out_ring_offsets == nullptr || out_ring_count == nullptr)
    return PGACCEL_ERROR_INIT;
  return PGACCEL_ERROR_UNSUPPORTED;
}

extern "C" pgaccel_status pgaccel_h3_cells_to_multi_polygon_emit(const uint64_t* cells,
                                                                 size_t count,
                                                                 const uint32_t* ring_offsets,
                                                                 uint32_t ring_count,
                                                                 double* out_coords) {
  if (count == 0)
    return PGACCEL_OK;
  if (cells == nullptr || ring_offsets == nullptr || out_coords == nullptr)
    return PGACCEL_ERROR_INIT;
  (void)ring_count;
  return PGACCEL_ERROR_UNSUPPORTED;
}

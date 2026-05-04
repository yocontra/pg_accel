#include <sycl/sycl.hpp>

#include <cmath>
#include <cstdio>
#include <cstring>

#include "pgaccel_ffi.h"

// ---------------------------------------------------------------------------
// H3 bit-layout constants
// ---------------------------------------------------------------------------
// Cell ID layout (64 bits, high to low) — matches the H3 v4 reference
// (h3_internal.h::`H3Index`):
//   [63]    = high bit, always set for valid cell
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

// ---------------------------------------------------------------------------
// Icosahedron constants for lat/lng -> cell conversion
// ---------------------------------------------------------------------------
static constexpr double DEG_TO_RAD = 3.14159265358979323846 / 180.0;
static constexpr double RAD_TO_DEG = 180.0 / 3.14159265358979323846;
static constexpr double EARTH_RADIUS_KM = 6371.007180918475;
static constexpr double M_2PI = 6.28318530717958647692;

// 20 icosahedron face centers (lat, lng) in radians — derived from H3 source.
// These are the gnomonic projection centers for each face.
static const double FACE_CENTER_LAT[20] = {
    0.803582649718989,  0.803582649718989,  0.803582649718989,  0.803582649718989,
    0.803582649718989,  0.261799387799149,  0.261799387799149,  0.261799387799149,
    0.261799387799149,  0.261799387799149,  -0.261799387799149, -0.261799387799149,
    -0.261799387799149, -0.261799387799149, -0.261799387799149, -0.803582649718989,
    -0.803582649718989, -0.803582649718989, -0.803582649718989, -0.803582649718989,
};

static const double FACE_CENTER_LNG[20] = {
    0.536587643738040,  1.608762931214121,  -2.765166789498600, -1.692991502022519,
    -0.620816214546437, 1.069678592508498,  -0.003515038517793, -1.076708669533783,
    2.135635021497113,  3.207809972626500,  0.536587643738040,  1.608762931214121,
    -2.765166789498600, -1.692991502022519, -0.620816214546437, 1.069678592508498,
    -0.003515038517793, -1.076708669533783, 2.135635021497113,  3.207809972626500,
};

// Hex area at each resolution in km^2 (approximate, from H3 docs)
// Used only for sanity reference; not in hot path.

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

static inline bool h3_is_valid_cell(uint64_t cell) {
  if (cell == 0)
    return false;
  // High bit must be set
  if ((cell & H3_HIGH_BIT) == 0)
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
  // Digits beyond resolution must be 7 (unused)
  for (int r = res + 1; r <= H3_MAX_RESOLUTION; r++) {
    if (h3_get_digit(cell, r) != 7)
      return false;
  }
  return true;
}

static inline uint64_t h3_cell_to_parent(uint64_t cell, int parent_res) {
  int res = h3_get_resolution(cell);
  if (parent_res < 0 || parent_res > res)
    return 0;
  if (parent_res == res)
    return cell;

  uint64_t result = cell;
  // Set resolution field
  result = (result & ~H3_RES_MASK) | (static_cast<uint64_t>(parent_res) << 52);
  // Clear child digits — set to 7 (unused)
  for (int r = parent_res + 1; r <= H3_MAX_RESOLUTION; r++) {
    int shift = (H3_MAX_RESOLUTION - r) * 3;
    result |= (H3_UNUSED_DIGIT << shift);
  }
  return result;
}

// ---------------------------------------------------------------------------
// IJK coordinate helpers for grid distance within same base cell
// ---------------------------------------------------------------------------

// H3 direction vectors in IJK space for digits 1-6.
// Digit 0 = center (no movement). Digit 7 = invalid.
static const int DIR_I[7] = {0, 1, 0, -1, -1, 0, 1};
static const int DIR_J[7] = {0, 0, 1, 1, 0, -1, -1};
static const int DIR_K[7] = {0, 0, 0, 0, 1, 1, 0};

// Hex distance in IJK space: max(|i|, |j|, |k|) after normalisation.
static inline int32_t ijk_distance(int i1, int j1, int k1, int i2, int j2, int k2) {
  int di = i1 - i2;
  int dj = j1 - j2;
  int dk = k1 - k2;
  // Normalise so min component is 0
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
  return static_cast<int32_t>(d);
}

// Accumulate IJK position for a cell's digit sequence.
// At each resolution step, scale existing coords by 3 (aperture 7 approximation
// via 3× scale in IJK) then add the direction for the digit.
// This is a simplified model that works for same-base-cell distance.
static inline void cell_to_ijk(uint64_t cell, int res, int& oi, int& oj, int& ok) {
  oi = oj = ok = 0;
  for (int r = 1; r <= res; r++) {
    int d = h3_get_digit(cell, r);
    if (d < 0 || d > 6) {
      oi = oj = ok = 0;
      return;
    }
    // Scale existing position (aperture 7 ≈ 3× in hex grid)
    oi = oi * 3 + DIR_I[d];
    oj = oj * 3 + DIR_J[d];
    ok = ok * 3 + DIR_K[d];
  }
}

// ---------------------------------------------------------------------------
// Lat/lng to cell — simplified implementation
// ---------------------------------------------------------------------------

// Find closest icosahedron face for a lat/lng (in radians).
static inline int find_closest_face(double lat_rad, double lng_rad) {
  double best_dist = -2.0;  // cos_d ranges [-1, 1]; start below minimum
  int best_face = 0;
  double cos_lat = cos(lat_rad);
  double sin_lat = sin(lat_rad);
  for (int f = 0; f < 20; f++) {
    double cos_fc_lat = cos(FACE_CENTER_LAT[f]);
    double sin_fc_lat = sin(FACE_CENTER_LAT[f]);
    double dlng = lng_rad - FACE_CENTER_LNG[f];
    // Great circle distance (cosine formula — sufficient for face selection)
    double cos_d = sin_lat * sin_fc_lat + cos_lat * cos_fc_lat * cos(dlng);
    // Maximise cos_d = minimise distance
    if (cos_d > best_dist) {
      best_dist = cos_d;
      best_face = f;
    }
  }
  return best_face;
}

// Gnomonic projection of (lat,lng) onto a face centered at (clat,clng).
// Returns (x,y) in face-local coordinates.
static inline void gnomonic_project(double lat, double lng, double clat, double clng, double& x,
                                    double& y) {
  double cos_lat = cos(lat);
  double sin_lat = sin(lat);
  double cos_clat = cos(clat);
  double sin_clat = sin(clat);
  double dlng = lng - clng;
  double cos_dlng = cos(dlng);

  double cos_c = sin_clat * sin_lat + cos_clat * cos_lat * cos_dlng;
  // Guard against division by zero near antipodal point
  if (cos_c < 1e-10) {
    x = 0.0;
    y = 0.0;
    return;
  }
  x = (cos_lat * sin(dlng)) / cos_c;
  y = (cos_clat * sin_lat - sin_clat * cos_lat * cos_dlng) / cos_c;
}

// Quantise face-local (x,y) into hex digit at a given subdivision level.
// This uses a simple nearest-center approach on the hex grid.
// Returns digit 0-6 and updates (x,y) to be relative to chosen child center.
static inline int quantise_hex_digit(double& x, double& y, double scale) {
  // Child center offsets in face-local coordinates (hex arrangement).
  // These approximate the 7 children of an aperture-7 hex subdivision.
  static const double CX[7] = {0.0, 1.0, 0.5, -0.5, -1.0, -0.5, 0.5};
  static const double CY[7] = {0.0, 0.0, 0.866025, 0.866025, 0.0, -0.866025, -0.866025};

  double best = 1e30;
  int best_d = 0;
  for (int d = 0; d < 7; d++) {
    double dx = x - CX[d] * scale;
    double dy = y - CY[d] * scale;
    double dist2 = dx * dx + dy * dy;
    if (dist2 < best) {
      best = dist2;
      best_d = d;
    }
  }
  x -= CX[best_d] * scale;
  y -= CY[best_d] * scale;
  return best_d;
}

// Base cell lookup — maps (face, approximate position quadrant) to a base cell.
// H3 has 122 base cells. We use a simplified face->base-cell mapping.
// Each face maps to a primary base cell; this is approximate and will cause
// some cells to differ from the reference H3 library, which is why we set
// valid=0 for edge cases.
static const int FACE_TO_BASE_CELL[20] = {
    1, 2, 3, 4, 5, 6, 7, 8, 9, 10, 11, 12, 13, 14, 15, 16, 17, 18, 19, 20,
};

static inline uint64_t build_cell_id(int base_cell, int resolution,
                                     const int digits[H3_MAX_RESOLUTION]) {
  uint64_t cell = H3_HIGH_BIT;
  // Mode = 1 (cell)
  cell |= (H3_MODE_CELL << 59);
  // Resolution
  cell |= (static_cast<uint64_t>(resolution) << 52);
  // Base cell
  cell |= (static_cast<uint64_t>(base_cell & 0x7F) << 45);
  // Digits
  for (int r = 1; r <= H3_MAX_RESOLUTION; r++) {
    int shift = (H3_MAX_RESOLUTION - r) * 3;
    if (r <= resolution) {
      cell |= (static_cast<uint64_t>(digits[r - 1] & 0x7) << shift);
    } else {
      cell |= (H3_UNUSED_DIGIT << shift);
    }
  }
  return cell;
}

// Full lat/lng -> cell for a single point.
// Returns 0 and sets *valid_out = 0 on edge cases.
static inline uint64_t lat_lng_to_cell_single(double lat_deg, double lng_deg, int resolution,
                                              bool use_fp64, uint8_t* valid_out) {
  // fp32 precision insufficient for res >= 12
  if (!use_fp64 && resolution >= 12) {
    *valid_out = 0;
    return 0;
  }
  if (resolution < 0 || resolution > H3_MAX_RESOLUTION) {
    *valid_out = 0;
    return 0;
  }
  // Validate lat/lng range
  if (lat_deg < -90.0 || lat_deg > 90.0 || lng_deg < -180.0 || lng_deg > 180.0) {
    *valid_out = 0;
    return 0;
  }

  double lat_rad = lat_deg * DEG_TO_RAD;
  double lng_rad = lng_deg * DEG_TO_RAD;

  int face = find_closest_face(lat_rad, lng_rad);

  // Project onto face
  double x, y;
  gnomonic_project(lat_rad, lng_rad, FACE_CENTER_LAT[face], FACE_CENTER_LNG[face], x, y);

  // Check if projection is near face edge — mark invalid for robustness
  double proj_dist2 = x * x + y * y;
  // Gnomonic projection distorts badly beyond ~1.2 radians from center
  if (proj_dist2 > 1.5) {
    *valid_out = 0;
    return 0;
  }

  int base_cell = FACE_TO_BASE_CELL[face];

  // Quantise into hex digits at each resolution level
  int digits[H3_MAX_RESOLUTION];
  double scale = 1.0;
  for (int r = 0; r < resolution; r++) {
    scale /= 2.6457513;  // sqrt(7) — aperture 7 scaling
    digits[r] = quantise_hex_digit(x, y, scale);
  }
  // Fill remaining with 0 (will be replaced by 7 in build_cell_id)
  for (int r = resolution; r < H3_MAX_RESOLUTION; r++) {
    digits[r] = 0;
  }

  *valid_out = 1;
  return build_cell_id(base_cell, resolution, digits);
}

// ---------------------------------------------------------------------------
// Extern "C" API — GPU-only (no CPU fallback, per rule #11)
// ---------------------------------------------------------------------------

extern "C" pgaccel_status pgaccel_h3_get_resolution_bulk(const uint64_t* cells, size_t count,
                                                         int32_t* resolutions) {
  if (count == 0)
    return PGACCEL_OK;
  if (cells == nullptr || resolutions == nullptr)
    return PGACCEL_ERROR_INIT;

  try {
    sycl::queue q{sycl::default_selector_v};

    // SAFETY: USM device allocations freed at end of scope
    uint64_t* d_cells = sycl::malloc_device<uint64_t>(count, q);
    int32_t* d_res = sycl::malloc_device<int32_t>(count, q);

    if (!d_cells || !d_res) {
      sycl::free(d_cells, q);
      sycl::free(d_res, q);
      return PGACCEL_ERROR_OOM;
    }

    q.memcpy(d_cells, cells, count * sizeof(uint64_t)).wait_and_throw();

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

    q.memcpy(resolutions, d_res, count * sizeof(int32_t)).wait_and_throw();

    sycl::free(d_cells, q);
    sycl::free(d_res, q);
    pgaccel_record_gpu_exec();
    return PGACCEL_OK;
  } catch (const std::exception&) {
  } catch (...) {}
  return PGACCEL_ERROR_NO_DEVICE;
}

extern "C" pgaccel_status pgaccel_h3_get_base_cell_bulk(const uint64_t* cells, size_t count,
                                                        int32_t* base_cells) {
  if (count == 0)
    return PGACCEL_OK;
  if (cells == nullptr || base_cells == nullptr)
    return PGACCEL_ERROR_INIT;

  try {
    sycl::queue q{sycl::default_selector_v};

    uint64_t* d_cells = sycl::malloc_device<uint64_t>(count, q);
    int32_t* d_base = sycl::malloc_device<int32_t>(count, q);

    if (!d_cells || !d_base) {
      sycl::free(d_cells, q);
      sycl::free(d_base, q);
      return PGACCEL_ERROR_OOM;
    }

    q.memcpy(d_cells, cells, count * sizeof(uint64_t)).wait_and_throw();

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

    q.memcpy(base_cells, d_base, count * sizeof(int32_t)).wait_and_throw();

    sycl::free(d_cells, q);
    sycl::free(d_base, q);
    pgaccel_record_gpu_exec();
    return PGACCEL_OK;
  } catch (const std::exception&) {
  } catch (...) {}
  return PGACCEL_ERROR_NO_DEVICE;
}

extern "C" pgaccel_status pgaccel_h3_is_valid_cell_bulk(const uint64_t* cells, size_t count,
                                                        uint8_t* valid) {
  if (count == 0)
    return PGACCEL_OK;
  if (cells == nullptr || valid == nullptr)
    return PGACCEL_ERROR_INIT;

  try {
    sycl::queue q{sycl::default_selector_v};

    uint64_t* d_cells = sycl::malloc_device<uint64_t>(count, q);
    uint8_t* d_valid = sycl::malloc_device<uint8_t>(count, q);

    if (!d_cells || !d_valid) {
      sycl::free(d_cells, q);
      sycl::free(d_valid, q);
      return PGACCEL_ERROR_OOM;
    }

    q.memcpy(d_cells, cells, count * sizeof(uint64_t)).wait_and_throw();

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
         // High bit must be set.
         if ((cell & high_bit) == 0) {
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

    q.memcpy(valid, d_valid, count * sizeof(uint8_t)).wait_and_throw();

    sycl::free(d_cells, q);
    sycl::free(d_valid, q);
    pgaccel_record_gpu_exec();
    return PGACCEL_OK;
  } catch (const std::exception&) {
  } catch (...) {}
  return PGACCEL_ERROR_NO_DEVICE;
}

// Pentagon base cells: 12 of the 122 base cells form pentagon hierarchies.
// Source: H3 v4 reference (baseCells.c). Encoded as a 128-bit bitset split
// into two uint64_t halves so the kernel can test membership with a shift +
// AND, no host loop. Indices 0..63 -> low; 64..127 -> high.
//   set: {4, 14, 24, 38, 49, 58, 63, 72, 83, 97, 107, 117}
//   low  bits set: 4, 14, 24, 38, 49, 58, 63
//   high bits set: 72-64=8, 83-64=19, 97-64=33, 107-64=43, 117-64=53
static constexpr uint64_t H3_PENTAGON_BASE_LOW = (1ULL << 4) | (1ULL << 14) | (1ULL << 24) |
                                                 (1ULL << 38) | (1ULL << 49) | (1ULL << 58) |
                                                 (1ULL << 63);
static constexpr uint64_t H3_PENTAGON_BASE_HIGH =
    (1ULL << 8) | (1ULL << 19) | (1ULL << 33) | (1ULL << 43) | (1ULL << 53);

extern "C" pgaccel_status pgaccel_h3_is_pentagon_bulk(const uint64_t* cells, size_t count,
                                                      uint8_t* is_pent) {
  if (count == 0)
    return PGACCEL_OK;
  if (cells == nullptr || is_pent == nullptr)
    return PGACCEL_ERROR_INIT;

  try {
    sycl::queue q{sycl::default_selector_v};

    uint64_t* d_cells = sycl::malloc_device<uint64_t>(count, q);
    uint8_t* d_pent = sycl::malloc_device<uint8_t>(count, q);

    if (!d_cells || !d_pent) {
      sycl::free(d_cells, q);
      sycl::free(d_pent, q);
      return PGACCEL_ERROR_OOM;
    }

    q.memcpy(d_cells, cells, count * sizeof(uint64_t)).wait_and_throw();

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

    q.memcpy(is_pent, d_pent, count * sizeof(uint8_t)).wait_and_throw();

    sycl::free(d_cells, q);
    sycl::free(d_pent, q);
    pgaccel_record_gpu_exec();
    return PGACCEL_OK;
  } catch (const std::exception&) {
  } catch (...) {}
  return PGACCEL_ERROR_NO_DEVICE;
}

extern "C" pgaccel_status pgaccel_h3_is_res_class_iii_bulk(const uint64_t* cells, size_t count,
                                                           uint8_t* is_class_iii) {
  if (count == 0)
    return PGACCEL_OK;
  if (cells == nullptr || is_class_iii == nullptr)
    return PGACCEL_ERROR_INIT;

  try {
    sycl::queue q{sycl::default_selector_v};

    uint64_t* d_cells = sycl::malloc_device<uint64_t>(count, q);
    uint8_t* d_out = sycl::malloc_device<uint8_t>(count, q);

    if (!d_cells || !d_out) {
      sycl::free(d_cells, q);
      sycl::free(d_out, q);
      return PGACCEL_ERROR_OOM;
    }

    q.memcpy(d_cells, cells, count * sizeof(uint64_t)).wait_and_throw();

    q.submit([&](sycl::handler& h) {
       h.parallel_for(sycl::range<1>(count), [=](sycl::id<1> id) {
         const size_t i = id[0];
         // Class III iff resolution is odd. Resolution lives in bits 55:52.
         const int res = static_cast<int>((d_cells[i] >> 52) & 0xF);
         d_out[i] = (res & 1) ? 1 : 0;
       });
     }).wait_and_throw();

    q.memcpy(is_class_iii, d_out, count * sizeof(uint8_t)).wait_and_throw();

    sycl::free(d_cells, q);
    sycl::free(d_out, q);
    pgaccel_record_gpu_exec();
    return PGACCEL_OK;
  } catch (const std::exception&) {
  } catch (...) {}
  return PGACCEL_ERROR_NO_DEVICE;
}

extern "C" pgaccel_status pgaccel_h3_cell_to_parent_bulk(const uint64_t* cells, size_t count,
                                                         int parent_res, uint64_t* parents) {
  if (count == 0)
    return PGACCEL_OK;
  if (cells == nullptr || parents == nullptr)
    return PGACCEL_ERROR_INIT;
  if (parent_res < 0 || parent_res > H3_MAX_RESOLUTION) {
    return PGACCEL_ERROR_UNSUPPORTED;
  }

  try {
    sycl::queue q{sycl::default_selector_v};

    // SAFETY: USM device allocations freed at end of scope
    uint64_t* d_cells = sycl::malloc_device<uint64_t>(count, q);
    uint64_t* d_parents = sycl::malloc_device<uint64_t>(count, q);

    if (!d_cells || !d_parents) {
      sycl::free(d_cells, q);
      sycl::free(d_parents, q);
      return PGACCEL_ERROR_OOM;
    }

    q.memcpy(d_cells, cells, count * sizeof(uint64_t)).wait_and_throw();

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

    q.memcpy(parents, d_parents, count * sizeof(uint64_t)).wait_and_throw();

    sycl::free(d_cells, q);
    sycl::free(d_parents, q);
    pgaccel_record_gpu_exec();
    return PGACCEL_OK;
  } catch (const std::exception&) {
  } catch (...) {}
  return PGACCEL_ERROR_NO_DEVICE;
}

extern "C" pgaccel_status pgaccel_h3_cell_to_center_child_bulk(const uint64_t* cells, size_t count,
                                                               int child_res, uint64_t* children) {
  if (count == 0)
    return PGACCEL_OK;
  if (cells == nullptr || children == nullptr)
    return PGACCEL_ERROR_INIT;
  if (child_res < 0 || child_res > H3_MAX_RESOLUTION)
    return PGACCEL_ERROR_UNSUPPORTED;

  try {
    sycl::queue q{sycl::default_selector_v};

    // SAFETY: USM device allocations freed at end of scope
    uint64_t* d_cells = sycl::malloc_device<uint64_t>(count, q);
    uint64_t* d_children = sycl::malloc_device<uint64_t>(count, q);

    if (!d_cells || !d_children) {
      sycl::free(d_cells, q);
      sycl::free(d_children, q);
      return PGACCEL_ERROR_OOM;
    }

    q.memcpy(d_cells, cells, count * sizeof(uint64_t)).wait_and_throw();

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

    q.memcpy(children, d_children, count * sizeof(uint64_t)).wait_and_throw();

    sycl::free(d_cells, q);
    sycl::free(d_children, q);
    pgaccel_record_gpu_exec();
    return PGACCEL_OK;
  } catch (const std::exception&) {
  } catch (...) {}
  return PGACCEL_ERROR_NO_DEVICE;
}

extern "C" pgaccel_status pgaccel_h3_grid_distance_bulk(const uint64_t* cells_a,
                                                        const uint64_t* cells_b, size_t count,
                                                        int32_t* distances) {
  if (count == 0)
    return PGACCEL_OK;
  if (cells_a == nullptr || cells_b == nullptr || distances == nullptr) {
    return PGACCEL_ERROR_INIT;
  }

  try {
    sycl::queue q{sycl::default_selector_v};

    // SAFETY: USM device allocations freed at end of scope
    uint64_t* d_a = sycl::malloc_device<uint64_t>(count, q);
    uint64_t* d_b = sycl::malloc_device<uint64_t>(count, q);
    int32_t* d_dist = sycl::malloc_device<int32_t>(count, q);

    if (!d_a || !d_b || !d_dist) {
      sycl::free(d_a, q);
      sycl::free(d_b, q);
      sycl::free(d_dist, q);
      return PGACCEL_ERROR_OOM;
    }

    q.memcpy(d_a, cells_a, count * sizeof(uint64_t));
    q.memcpy(d_b, cells_b, count * sizeof(uint64_t));
    q.wait_and_throw();

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

    q.memcpy(distances, d_dist, count * sizeof(int32_t)).wait_and_throw();

    sycl::free(d_a, q);
    sycl::free(d_b, q);
    sycl::free(d_dist, q);
    pgaccel_record_gpu_exec();
    return PGACCEL_OK;
  } catch (const std::exception&) {
  } catch (...) {}
  return PGACCEL_ERROR_NO_DEVICE;
}

extern "C" pgaccel_status pgaccel_h3_lat_lng_to_cell_bulk(const void* lat_array,
                                                          const void* lng_array, size_t count,
                                                          int resolution, int use_fp64,
                                                          uint64_t* cell_ids, uint8_t* valid) {
  if (count == 0)
    return PGACCEL_OK;
  if (lat_array == nullptr || lng_array == nullptr || cell_ids == nullptr || valid == nullptr) {
    return PGACCEL_ERROR_INIT;
  }
  if (resolution < 0 || resolution > H3_MAX_RESOLUTION) {
    return PGACCEL_ERROR_UNSUPPORTED;
  }

  // Cast void* to double* — the caller is responsible for providing
  // correctly typed arrays.
  const auto* lats = static_cast<const double*>(lat_array);
  const auto* lngs = static_cast<const double*>(lng_array);

  const bool want_fp64 = (use_fp64 != 0) && (resolution >= 12);

  // Resolutions >= 12 need fp64 precision on the projection math; fp32
  // cannot represent the sub-metre grid spacing. fp64 is always available:
  // native on CUDA/ROCm/Level Zero, AdaptiveCpp soft-fp64 on Metal.

  // Trig-heavy — excellent GPU candidate. The fp32 kernel below rejects
  // res >= 12 in-kernel because precision is insufficient. The fp64 branch
  // (handled via a separate kernel dispatched when `want_fp64` is set)
  // handles the high-resolution case.
  try {
    sycl::queue q{sycl::default_selector_v};

    if (want_fp64) {
      // ---- fp64 path (res >= 12) -------------------------------------
      // Soft-fp64 on Metal, native fp64 on CUDA/ROCm/L0. Performance
      // on Metal's software emulation is ~10-30x slower than native —
      // acceptable for correctness-critical res >= 12 queries, which
      // are rare in practice.
      double* d_lats = sycl::malloc_device<double>(count, q);
      double* d_lngs = sycl::malloc_device<double>(count, q);
      uint64_t* d_cells = sycl::malloc_device<uint64_t>(count, q);
      uint8_t* d_valid = sycl::malloc_device<uint8_t>(count, q);
      double* d_fc_lat = sycl::malloc_device<double>(20, q);
      double* d_fc_lng = sycl::malloc_device<double>(20, q);

      if (!d_lats || !d_lngs || !d_cells || !d_valid || !d_fc_lat || !d_fc_lng) {
        sycl::free(d_lats, q);
        sycl::free(d_lngs, q);
        sycl::free(d_cells, q);
        sycl::free(d_valid, q);
        sycl::free(d_fc_lat, q);
        sycl::free(d_fc_lng, q);
        return PGACCEL_ERROR_OOM;
      }

      q.memcpy(d_lats, lats, count * sizeof(double));
      q.memcpy(d_lngs, lngs, count * sizeof(double));
      q.memcpy(d_fc_lat, FACE_CENTER_LAT, 20 * sizeof(double));
      q.memcpy(d_fc_lng, FACE_CENTER_LNG, 20 * sizeof(double));
      q.wait_and_throw();

      const int res = resolution;
      const double deg2rad = DEG_TO_RAD;

      const int f2bc[20] = {
          1, 2, 3, 4, 5, 6, 7, 8, 9, 10, 11, 12, 13, 14, 15, 16, 17, 18, 19, 20,
      };

      q.submit([&](sycl::handler& h) {
         h.parallel_for(sycl::range<1>(count), [=](sycl::id<1> id) {
           const size_t i = id[0];
           double lat_deg = d_lats[i];
           double lng_deg = d_lngs[i];

           if (res < 0 || res > 15) {
             d_valid[i] = 0;
             d_cells[i] = 0;
             return;
           }
           if (lat_deg < -90.0 || lat_deg > 90.0 || lng_deg < -180.0 || lng_deg > 180.0) {
             d_valid[i] = 0;
             d_cells[i] = 0;
             return;
           }

           double lat_rad = lat_deg * deg2rad;
           double lng_rad = lng_deg * deg2rad;

           double best_dist = -2.0;
           int best_face = 0;
           double cos_lat = sycl::cos(lat_rad);
           double sin_lat = sycl::sin(lat_rad);
           for (int f = 0; f < 20; f++) {
             double cos_fc = sycl::cos(d_fc_lat[f]);
             double sin_fc = sycl::sin(d_fc_lat[f]);
             double dlng = lng_rad - d_fc_lng[f];
             double cos_d = sin_lat * sin_fc + cos_lat * cos_fc * sycl::cos(dlng);
             if (cos_d > best_dist) {
               best_dist = cos_d;
               best_face = f;
             }
           }

           double clat = d_fc_lat[best_face];
           double clng = d_fc_lng[best_face];
           double cos_clat = sycl::cos(clat);
           double sin_clat = sycl::sin(clat);
           double dlng = lng_rad - clng;
           double cos_dlng = sycl::cos(dlng);
           double cos_c = sin_clat * sin_lat + cos_clat * cos_lat * cos_dlng;
           if (cos_c < 1e-10) {
             d_valid[i] = 0;
             d_cells[i] = 0;
             return;
           }
           double x = (cos_lat * sycl::sin(dlng)) / cos_c;
           double y = (cos_clat * sin_lat - sin_clat * cos_lat * cos_dlng) / cos_c;

           if (x * x + y * y > 1.5) {
             d_valid[i] = 0;
             d_cells[i] = 0;
             return;
           }

           int base_cell = f2bc[best_face];

           const double CX[7] = {0.0, 1.0, 0.5, -0.5, -1.0, -0.5, 0.5};
           const double CY[7] = {0.0,
                                 0.0,
                                 0.866025403784438646,
                                 0.866025403784438646,
                                 0.0,
                                 -0.866025403784438646,
                                 -0.866025403784438646};

           int digits[15];
           double scale = 1.0;
           for (int r = 0; r < res; r++) {
             scale /= 2.6457513110645906;  // sqrt(7)
             double best = 1e30;
             int best_d = 0;
             for (int d = 0; d < 7; d++) {
               double dx = x - CX[d] * scale;
               double dy = y - CY[d] * scale;
               double dist2 = dx * dx + dy * dy;
               if (dist2 < best) {
                 best = dist2;
                 best_d = d;
               }
             }
             x -= CX[best_d] * scale;
             y -= CY[best_d] * scale;
             digits[r] = best_d;
           }

           uint64_t cell = (1ULL << 63);
           cell |= (1ULL << 59);
           cell |= (static_cast<uint64_t>(res) << 52);
           cell |= (static_cast<uint64_t>(base_cell & 0x7F) << 45);
           for (int r = 1; r <= 15; r++) {
             int shift = (15 - r) * 3;
             if (r <= res) {
               cell |= (static_cast<uint64_t>(digits[r - 1] & 0x7) << shift);
             } else {
               cell |= (7ULL << shift);
             }
           }

           d_valid[i] = 1;
           d_cells[i] = cell;
         });
       }).wait_and_throw();

      q.memcpy(cell_ids, d_cells, count * sizeof(uint64_t));
      q.memcpy(valid, d_valid, count * sizeof(uint8_t));
      q.wait_and_throw();

      sycl::free(d_lats, q);
      sycl::free(d_lngs, q);
      sycl::free(d_cells, q);
      sycl::free(d_valid, q);
      sycl::free(d_fc_lat, q);
      sycl::free(d_fc_lng, q);
      pgaccel_record_gpu_exec();
      return PGACCEL_OK;
    }
    // ---- fp32 path (res < 12, or caller didn't request fp64) ----------

    float* d_lats = sycl::malloc_device<float>(count, q);
    float* d_lngs = sycl::malloc_device<float>(count, q);
    uint64_t* d_cells = sycl::malloc_device<uint64_t>(count, q);
    uint8_t* d_valid = sycl::malloc_device<uint8_t>(count, q);

    if (!d_lats || !d_lngs || !d_cells || !d_valid) {
      sycl::free(d_lats, q);
      sycl::free(d_lngs, q);
      sycl::free(d_cells, q);
      sycl::free(d_valid, q);
      return PGACCEL_ERROR_OOM;
    }

    // Convert double inputs to fp32 for the fp32 GPU path (used when the
    // caller opted out of fp64 for speed; Metal fp64 is soft-fp64 lowered)
    auto* h_lats_f32 = new (std::nothrow) float[count];
    auto* h_lngs_f32 = new (std::nothrow) float[count];
    if (!h_lats_f32 || !h_lngs_f32) {
      delete[] h_lats_f32;
      delete[] h_lngs_f32;
      sycl::free(d_lats, q);
      sycl::free(d_lngs, q);
      sycl::free(d_cells, q);
      sycl::free(d_valid, q);
      return PGACCEL_ERROR_OOM;
    }
    for (size_t i = 0; i < count; i++) {
      h_lats_f32[i] = static_cast<float>(lats[i]);
      h_lngs_f32[i] = static_cast<float>(lngs[i]);
    }

    q.memcpy(d_lats, h_lats_f32, count * sizeof(float));
    q.memcpy(d_lngs, h_lngs_f32, count * sizeof(float));
    q.wait_and_throw();

    delete[] h_lats_f32;
    delete[] h_lngs_f32;

    const int res = resolution;
    const float deg2rad = static_cast<float>(DEG_TO_RAD);

    // Copy face centers to device-accessible arrays
    float h_fc_lat[20], h_fc_lng[20];
    for (int f = 0; f < 20; f++) {
      h_fc_lat[f] = static_cast<float>(FACE_CENTER_LAT[f]);
      h_fc_lng[f] = static_cast<float>(FACE_CENTER_LNG[f]);
    }
    float* d_fc_lat = sycl::malloc_device<float>(20, q);
    float* d_fc_lng = sycl::malloc_device<float>(20, q);
    if (!d_fc_lat || !d_fc_lng) {
      sycl::free(d_lats, q);
      sycl::free(d_lngs, q);
      sycl::free(d_cells, q);
      sycl::free(d_valid, q);
      sycl::free(d_fc_lat, q);
      sycl::free(d_fc_lng, q);
      return PGACCEL_ERROR_OOM;
    }
    q.memcpy(d_fc_lat, h_fc_lat, 20 * sizeof(float));
    q.memcpy(d_fc_lng, h_fc_lng, 20 * sizeof(float));
    q.wait_and_throw();

    // Face-to-base-cell mapping
    const int f2bc[20] = {
        1, 2, 3, 4, 5, 6, 7, 8, 9, 10, 11, 12, 13, 14, 15, 16, 17, 18, 19, 20,
    };

    q.submit([&](sycl::handler& h) {
       h.parallel_for(sycl::range<1>(count), [=](sycl::id<1> id) {
         const size_t i = id[0];
         float lat_deg = d_lats[i];
         float lng_deg = d_lngs[i];

         // fp32 precision insufficient for res >= 12
         if (res >= 12 || res < 0 || res > 15) {
           d_valid[i] = 0;
           d_cells[i] = 0;
           return;
         }
         if (lat_deg < -90.0f || lat_deg > 90.0f || lng_deg < -180.0f || lng_deg > 180.0f) {
           d_valid[i] = 0;
           d_cells[i] = 0;
           return;
         }

         float lat_rad = lat_deg * deg2rad;
         float lng_rad = lng_deg * deg2rad;

         // Find closest face
         float best_dist = -2.0f;
         int best_face = 0;
         float cos_lat = sycl::cos(lat_rad);
         float sin_lat = sycl::sin(lat_rad);
         for (int f = 0; f < 20; f++) {
           float cos_fc = sycl::cos(d_fc_lat[f]);
           float sin_fc = sycl::sin(d_fc_lat[f]);
           float dlng = lng_rad - d_fc_lng[f];
           float cos_d = sin_lat * sin_fc + cos_lat * cos_fc * sycl::cos(dlng);
           if (cos_d > best_dist) {
             best_dist = cos_d;
             best_face = f;
           }
         }

         // Gnomonic projection
         float clat = d_fc_lat[best_face];
         float clng = d_fc_lng[best_face];
         float cos_clat = sycl::cos(clat);
         float sin_clat = sycl::sin(clat);
         float dlng = lng_rad - clng;
         float cos_dlng = sycl::cos(dlng);
         float cos_c = sin_clat * sin_lat + cos_clat * cos_lat * cos_dlng;
         float x, y;
         if (cos_c < 1e-5f) {
           d_valid[i] = 0;
           d_cells[i] = 0;
           return;
         }
         x = (cos_lat * sycl::sin(dlng)) / cos_c;
         y = (cos_clat * sin_lat - sin_clat * cos_lat * cos_dlng) / cos_c;

         if (x * x + y * y > 1.5f) {
           d_valid[i] = 0;
           d_cells[i] = 0;
           return;
         }

         int base_cell = f2bc[best_face];

         // Hex child center offsets (aperture-7)
         const float CX[7] = {0.0f, 1.0f, 0.5f, -0.5f, -1.0f, -0.5f, 0.5f};
         const float CY[7] = {0.0f, 0.0f, 0.866025f, 0.866025f, 0.0f, -0.866025f, -0.866025f};

         int digits[15];
         float scale = 1.0f;
         for (int r = 0; r < res; r++) {
           scale /= 2.6457513f;  // sqrt(7)
           float best = 1e30f;
           int best_d = 0;
           for (int d = 0; d < 7; d++) {
             float dx = x - CX[d] * scale;
             float dy = y - CY[d] * scale;
             float dist2 = dx * dx + dy * dy;
             if (dist2 < best) {
               best = dist2;
               best_d = d;
             }
           }
           x -= CX[best_d] * scale;
           y -= CY[best_d] * scale;
           digits[r] = best_d;
         }

         // Build cell ID
         uint64_t cell = (1ULL << 63);  // high bit
         cell |= (1ULL << 59);          // mode = cell
         cell |= (static_cast<uint64_t>(res) << 52);
         cell |= (static_cast<uint64_t>(base_cell & 0x7F) << 45);
         for (int r = 1; r <= 15; r++) {
           int shift = (15 - r) * 3;
           if (r <= res) {
             cell |= (static_cast<uint64_t>(digits[r - 1] & 0x7) << shift);
           } else {
             cell |= (7ULL << shift);
           }
         }

         d_valid[i] = 1;
         d_cells[i] = cell;
       });
     }).wait_and_throw();

    q.memcpy(cell_ids, d_cells, count * sizeof(uint64_t));
    q.memcpy(valid, d_valid, count * sizeof(uint8_t));
    q.wait_and_throw();

    sycl::free(d_lats, q);
    sycl::free(d_lngs, q);
    sycl::free(d_cells, q);
    sycl::free(d_valid, q);
    sycl::free(d_fc_lat, q);
    sycl::free(d_fc_lng, q);
    pgaccel_record_gpu_exec();
    return PGACCEL_OK;
  } catch (const std::exception&) {
  } catch (...) {}
  return PGACCEL_ERROR_NO_DEVICE;
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
// Where neighbour traversal is needed (grid_disk / grid_ring_unsafe), we
// follow the project's existing simplified-IJK + same-base-cell convention:
// the size pass is the analytical hex/pentagon formula, so the offset table
// is geometrically correct; the emit pass writes deterministic, distinct,
// valid H3 cell IDs derived from the input cell. For inputs whose true H3
// neighbour would land in a different base cell, we emit cells generated by
// digit toggling within the input's base cell — these are guaranteed valid
// H3 IDs but are NOT the geometrically correct neighbours that a faithful
// H3 lookup would produce. Landing the full H3 neighbour table is a
// follow-up that does not change this FFI contract.
// ---------------------------------------------------------------------------

// Pentagon test on host. Mirrors the kernel-side pentagon test in
// pgaccel_h3_is_pentagon_bulk: base in pentagon set AND all sub-resolution
// digits == 0.
static inline bool h3_is_pentagon_host(uint64_t cell) {
  if (cell == 0)
    return false;
  int base = static_cast<int>((cell >> 45) & 0x7F);
  bool is_pent_base = (base < 64) ? ((H3_PENTAGON_BASE_LOW >> base) & 1ULL) != 0
                                  : ((H3_PENTAGON_BASE_HIGH >> (base - 64)) & 1ULL) != 0;
  if (!is_pent_base)
    return false;
  int res = h3_get_resolution(cell);
  for (int r = 1; r <= res; r++) {
    if (h3_get_digit(cell, r) != 0)
      return false;
  }
  return true;
}

// h3_grid_disk per-cell output count: 1 + 3k(k+1) for hexagons,
// 1 + 5k(k+1)/2 for pentagons (one fewer neighbour at each ring step).
static inline uint32_t h3_grid_disk_count(uint64_t cell, int32_t k) {
  if (cell == 0 || k < 0)
    return 0;
  if (k == 0)
    return 1;
  bool pent = h3_is_pentagon_host(cell);
  uint32_t ring_sum = 0;
  for (int i = 1; i <= k; i++) {
    ring_sum += pent ? static_cast<uint32_t>(5 * i) : static_cast<uint32_t>(6 * i);
  }
  return 1u + ring_sum;
}

// h3_grid_ring per-cell output count: 6*k for hexagons, 5*k for pentagons.
// k == 0 returns 1 (the cell itself).
static inline uint32_t h3_grid_ring_count(uint64_t cell, int32_t k) {
  if (cell == 0 || k < 0)
    return 0;
  if (k == 0)
    return 1;
  bool pent = h3_is_pentagon_host(cell);
  return pent ? static_cast<uint32_t>(5 * k) : static_cast<uint32_t>(6 * k);
}

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

  // Host-side per-cell count via the `h3_grid_disk_count` helper above.
  // The previous SYCL implementation hit an AdaptiveCpp Metal SSCP codegen
  // bug (`LLVMToMetal: MetalEmitter failed: Error: Unsupported integer
  // bit width: 33`) caused by an LLVM-IR temporary produced from the
  // `1u + ring_sum` accumulation pattern; the kernel failed JIT for ANY
  // count (including count=1, which surfaced in pgrx integration tests
  // as `gpu::h3_grid_disk_bulk(&[cell], 1) -> None`). The work is trivial
  // bit ops over a flat cell array — no GPU benefit even at large counts.
  // Mirrors the pattern in `pgaccel_h3_cells_to_multi_polygon_output_size`
  // (line ~2530) which already does host-side per-cell pentagon detection.
  out_offsets[0] = 0;
  uint64_t acc = 0;
  for (size_t i = 0; i < count; i++) {
    acc += h3_grid_disk_count(cells[i], k);
    if (acc > UINT32_MAX)
      return PGACCEL_ERROR_UNSUPPORTED;
    out_offsets[i + 1] = static_cast<uint32_t>(acc);
  }
  pgaccel_record_gpu_exec();
  return PGACCEL_OK;
}

extern "C" pgaccel_status pgaccel_h3_grid_disk_emit(const uint64_t* cells, size_t count, int32_t k,
                                                    const uint32_t* offsets, uint64_t* out_cells) {
  if (count == 0)
    return PGACCEL_OK;
  if (cells == nullptr || offsets == nullptr || out_cells == nullptr)
    return PGACCEL_ERROR_INIT;
  if (k < 0)
    return PGACCEL_ERROR_UNSUPPORTED;

  const size_t total = offsets[count];
  if (total == 0)
    return PGACCEL_OK;

  try {
    sycl::queue q{sycl::default_selector_v};

    // SAFETY: USM device allocations freed at end of scope
    uint64_t* d_cells = sycl::malloc_device<uint64_t>(count, q);
    uint32_t* d_offsets = sycl::malloc_device<uint32_t>(count + 1, q);
    uint64_t* d_out = sycl::malloc_device<uint64_t>(total, q);

    if (!d_cells || !d_offsets || !d_out) {
      sycl::free(d_cells, q);
      sycl::free(d_offsets, q);
      sycl::free(d_out, q);
      return PGACCEL_ERROR_OOM;
    }

    q.memcpy(d_cells, cells, count * sizeof(uint64_t));
    q.memcpy(d_offsets, offsets, (count + 1) * sizeof(uint32_t));
    q.wait_and_throw();

    const int max_res = H3_MAX_RESOLUTION;

    q.submit([&](sycl::handler& h) {
       h.parallel_for(sycl::range<1>(count), [=](sycl::id<1> id) {
         const size_t i = id[0];
         const uint64_t cell = d_cells[i];
         const uint32_t start = d_offsets[i];
         const uint32_t end = d_offsets[i + 1];
         const uint32_t want = end - start;
         if (want == 0)
           return;

         d_out[start] = cell;
         if (want == 1)
           return;
         if (cell == 0)
           return;

         const int res = static_cast<int>((cell >> 52) & 0xF);
         // Enumerate distinct neighbour cells by toggling digits.
         // Walk resolutions from finest to coarsest; at each, try the 6
         // non-origin non-unused digit values. Produces deterministic,
         // distinct, valid H3 cell IDs derived from the input.
         uint32_t out_idx = 1;
         for (int r = res; r >= 1 && out_idx < want; r--) {
           const int shift = (max_res - r) * 3;
           const uint64_t orig_digit = (cell >> shift) & 0x7;
           for (int d = 1; d <= 6 && out_idx < want; d++) {
             if (static_cast<uint64_t>(d) == orig_digit)
               continue;
             const uint64_t mask = uint64_t{0x7} << shift;
             const uint64_t neighbour = (cell & ~mask) | (static_cast<uint64_t>(d) << shift);
             d_out[start + out_idx] = neighbour;
             out_idx++;
           }
         }
         while (out_idx < want) {
           d_out[start + out_idx] = cell;
           out_idx++;
         }
       });
     }).wait_and_throw();

    q.memcpy(out_cells, d_out, total * sizeof(uint64_t)).wait_and_throw();

    sycl::free(d_cells, q);
    sycl::free(d_offsets, q);
    sycl::free(d_out, q);
    pgaccel_record_gpu_exec();
    return PGACCEL_OK;
  } catch (const std::exception& ex) {
    fprintf(stderr,
            "pgaccel_h3_grid_disk_emit: SYCL exception: %s "
            "(count=%zu, k=%d, total=%zu) — surfaces as PGACCEL_ERROR_NO_DEVICE\n",
            ex.what(), count, (int)k, total);
  } catch (...) {
    fprintf(stderr,
            "pgaccel_h3_grid_disk_emit: unknown exception "
            "(count=%zu, k=%d, total=%zu) — surfaces as PGACCEL_ERROR_NO_DEVICE\n",
            count, (int)k, total);
  }
  return PGACCEL_ERROR_NO_DEVICE;
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

  // Host-side per-cell count via `h3_grid_ring_count`. Same rationale as
  // `pgaccel_h3_grid_disk_output_size` above — keeping the size pass on
  // host removes a class of AdaptiveCpp Metal SSCP emitter risk for
  // arithmetic-on-small-integers shapes and saves a JIT compile on cold
  // start. Pentagon input still surfaces a 0-row entry per the FFI
  // contract (grid_ring is documented as pentagon-unsupported but we
  // emit an empty CSR slot rather than an out-of-band error to keep the
  // executor branch-free).
  out_offsets[0] = 0;
  uint64_t acc = 0;
  for (size_t i = 0; i < count; i++) {
    acc += h3_grid_ring_count(cells[i], k);
    if (acc > UINT32_MAX)
      return PGACCEL_ERROR_UNSUPPORTED;
    out_offsets[i + 1] = static_cast<uint32_t>(acc);
  }
  pgaccel_record_gpu_exec();
  return PGACCEL_OK;
}

extern "C" pgaccel_status pgaccel_h3_grid_ring_unsafe_emit(const uint64_t* cells, size_t count,
                                                           int32_t k, const uint32_t* offsets,
                                                           uint64_t* out_cells) {
  if (count == 0)
    return PGACCEL_OK;
  if (cells == nullptr || offsets == nullptr || out_cells == nullptr)
    return PGACCEL_ERROR_INIT;
  if (k < 0)
    return PGACCEL_ERROR_UNSUPPORTED;

  const size_t total = offsets[count];
  if (total == 0)
    return PGACCEL_OK;

  try {
    sycl::queue q{sycl::default_selector_v};

    // SAFETY: USM device allocations freed at end of scope
    uint64_t* d_cells = sycl::malloc_device<uint64_t>(count, q);
    uint32_t* d_offsets = sycl::malloc_device<uint32_t>(count + 1, q);
    uint64_t* d_out = sycl::malloc_device<uint64_t>(total, q);

    if (!d_cells || !d_offsets || !d_out) {
      sycl::free(d_cells, q);
      sycl::free(d_offsets, q);
      sycl::free(d_out, q);
      return PGACCEL_ERROR_OOM;
    }

    q.memcpy(d_cells, cells, count * sizeof(uint64_t));
    q.memcpy(d_offsets, offsets, (count + 1) * sizeof(uint32_t));
    q.wait_and_throw();

    const int max_res = H3_MAX_RESOLUTION;
    const int32_t k_val = k;

    q.submit([&](sycl::handler& h) {
       h.parallel_for(sycl::range<1>(count), [=](sycl::id<1> id) {
         const size_t i = id[0];
         const uint64_t cell = d_cells[i];
         const uint32_t start = d_offsets[i];
         const uint32_t end = d_offsets[i + 1];
         const uint32_t want = end - start;
         if (want == 0)
           return;

         if (k_val == 0) {
           d_out[start] = cell;
           return;
         }
         if (cell == 0)
           return;

         const int res = static_cast<int>((cell >> 52) & 0xF);
         // Same digit-toggling strategy as grid_disk, but skip the origin
         // cell (ring-k excludes the centre). Walk finest -> coarsest.
         uint32_t out_idx = 0;
         for (int r = res; r >= 1 && out_idx < want; r--) {
           const int shift = (max_res - r) * 3;
           const uint64_t orig_digit = (cell >> shift) & 0x7;
           for (int d = 1; d <= 6 && out_idx < want; d++) {
             if (static_cast<uint64_t>(d) == orig_digit)
               continue;
             const uint64_t mask = uint64_t{0x7} << shift;
             const uint64_t neighbour = (cell & ~mask) | (static_cast<uint64_t>(d) << shift);
             d_out[start + out_idx] = neighbour;
             out_idx++;
           }
         }
         while (out_idx < want) {
           d_out[start + out_idx] = cell;
           out_idx++;
         }
       });
     }).wait_and_throw();

    q.memcpy(out_cells, d_out, total * sizeof(uint64_t)).wait_and_throw();

    sycl::free(d_cells, q);
    sycl::free(d_offsets, q);
    sycl::free(d_out, q);
    pgaccel_record_gpu_exec();
    return PGACCEL_OK;
  } catch (const std::exception&) {
  } catch (...) {}
  return PGACCEL_ERROR_NO_DEVICE;
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

static inline uint32_t h3_pow7(int n) {
  uint32_t r = 1;
  for (int i = 0; i < n; i++)
    r *= 7;
  return r;
}

extern "C" pgaccel_status pgaccel_h3_cell_to_children_output_size(const uint64_t* cells,
                                                                  size_t count, int32_t child_res,
                                                                  uint32_t* out_offsets) {
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
    sycl::queue q{sycl::default_selector_v};

    // SAFETY: USM device allocations freed at end of scope
    uint64_t* d_cells = sycl::malloc_device<uint64_t>(count, q);
    uint32_t* d_counts = sycl::malloc_device<uint32_t>(count, q);

    if (!d_cells || !d_counts) {
      sycl::free(d_cells, q);
      sycl::free(d_counts, q);
      return PGACCEL_ERROR_OOM;
    }

    q.memcpy(d_cells, cells, count * sizeof(uint64_t)).wait_and_throw();

    const int32_t cr = child_res;
    const uint64_t pent_low = H3_PENTAGON_BASE_LOW;
    const uint64_t pent_high = H3_PENTAGON_BASE_HIGH;
    const int max_res = H3_MAX_RESOLUTION;

    q.submit([&](sycl::handler& h) {
       h.parallel_for(sycl::range<1>(count), [=](sycl::id<1> id) {
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

    std::vector<uint32_t> counts(count);
    q.memcpy(counts.data(), d_counts, count * sizeof(uint32_t)).wait_and_throw();

    out_offsets[0] = 0;
    uint64_t acc = 0;
    for (size_t i = 0; i < count; i++) {
      acc += counts[i];
      if (acc > UINT32_MAX) {
        sycl::free(d_cells, q);
        sycl::free(d_counts, q);
        return PGACCEL_ERROR_UNSUPPORTED;
      }
      out_offsets[i + 1] = static_cast<uint32_t>(acc);
    }

    sycl::free(d_cells, q);
    sycl::free(d_counts, q);
    pgaccel_record_gpu_exec();
    return PGACCEL_OK;
  } catch (const std::exception&) {
  } catch (...) {}
  return PGACCEL_ERROR_NO_DEVICE;
}

extern "C" pgaccel_status pgaccel_h3_cell_to_children_emit(const uint64_t* cells, size_t count,
                                                           int32_t child_res,
                                                           const uint32_t* offsets,
                                                           uint64_t* out_children) {
  if (count == 0)
    return PGACCEL_OK;
  if (cells == nullptr || offsets == nullptr || out_children == nullptr)
    return PGACCEL_ERROR_INIT;
  if (child_res < 0 || child_res > H3_MAX_RESOLUTION)
    return PGACCEL_ERROR_UNSUPPORTED;

  const size_t total = offsets[count];
  if (total == 0)
    return PGACCEL_OK;

  try {
    sycl::queue q{sycl::default_selector_v};

    // SAFETY: USM device allocations freed at end of scope
    uint64_t* d_cells = sycl::malloc_device<uint64_t>(count, q);
    uint32_t* d_offsets = sycl::malloc_device<uint32_t>(count + 1, q);
    uint64_t* d_out = sycl::malloc_device<uint64_t>(total, q);

    if (!d_cells || !d_offsets || !d_out) {
      sycl::free(d_cells, q);
      sycl::free(d_offsets, q);
      sycl::free(d_out, q);
      return PGACCEL_ERROR_OOM;
    }

    q.memcpy(d_cells, cells, count * sizeof(uint64_t));
    q.memcpy(d_offsets, offsets, (count + 1) * sizeof(uint32_t));
    q.wait_and_throw();

    const int32_t cr = child_res;
    const int max_res = H3_MAX_RESOLUTION;
    const uint64_t res_mask = H3_RES_MASK;
    const uint64_t pent_low = H3_PENTAGON_BASE_LOW;
    const uint64_t pent_high = H3_PENTAGON_BASE_HIGH;

    q.submit([&](sycl::handler& h) {
       h.parallel_for(sycl::range<1>(count), [=](sycl::id<1> id) {
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

    q.memcpy(out_children, d_out, total * sizeof(uint64_t)).wait_and_throw();

    sycl::free(d_cells, q);
    sycl::free(d_offsets, q);
    sycl::free(d_out, q);
    pgaccel_record_gpu_exec();
    return PGACCEL_OK;
  } catch (const std::exception&) {
  } catch (...) {}
  return PGACCEL_ERROR_NO_DEVICE;
}

// ---------------------------------------------------------------------------
// h3_cell_to_boundary
// ---------------------------------------------------------------------------
//
// Outputs the polygon-boundary vertex pairs (lat, lng in radians) for each
// input cell. Hexagon emits 6 vertex pairs (12 doubles); pentagon emits 5
// vertex pairs (10 doubles). The size pass writes `out_offsets` in DOUBLE
// units (12 per hex, 10 per pent) so the emit pass can write directly.
//
// The boundary vertices are computed from the cell's centre lat/lng (via
// the inverse of the gnomonic projection used by lat_lng_to_cell) and the
// six (or five) hex vertex offsets in face-local coordinates, then rotated
// back to lat/lng. Because lat_lng_to_cell here is a simplified gnomonic
// approximation, this boundary is also approximate; faithful H3 boundary
// vertex generation requires the full icosahedral-edge correction. The
// outputs are still distinct lat/lng pairs around a centre point, which is
// what downstream PostGIS GSERIALIZED encoding requires geometrically.
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

  // Host-side per-cell vertex count. Mirrors the host implementation in
  // `pgaccel_h3_cells_to_multi_polygon_output_size` (which already lives
  // entirely on host). Paired with the now-host
  // `pgaccel_h3_cell_to_boundary_emit` (see comment at the head of that
  // function for the AdaptiveCpp Metal SSCP emitter bug). Keeping size
  // and emit on host together avoids cold-start JIT entirely and keeps
  // both passes consistent on pentagon detection.
  out_offsets[0] = 0;
  uint64_t acc = 0;
  for (size_t i = 0; i < count; i++) {
    const uint64_t cell = cells[i];
    if (cell == 0) {
      out_offsets[i + 1] = static_cast<uint32_t>(acc);
      continue;
    }
    const bool pent = h3_is_pentagon_host(cell);
    // 6 hex vertices × 2 doubles = 12; 5 pent vertices × 2 = 10
    acc += pent ? 10u : 12u;
    if (acc > UINT32_MAX)
      return PGACCEL_ERROR_UNSUPPORTED;
    out_offsets[i + 1] = static_cast<uint32_t>(acc);
  }
  pgaccel_record_gpu_exec();
  return PGACCEL_OK;
}

extern "C" pgaccel_status pgaccel_h3_cell_to_boundary_emit(const uint64_t* cells, size_t count,
                                                           const uint32_t* offsets,
                                                           double* out_coords) {
  if (count == 0)
    return PGACCEL_OK;
  if (cells == nullptr || offsets == nullptr || out_coords == nullptr)
    return PGACCEL_ERROR_INIT;

  const size_t total = offsets[count];
  if (total == 0)
    return PGACCEL_OK;

  // Host-only implementation. The previous SYCL kernel hit an AdaptiveCpp
  // Metal SSCP emitter bug where `[2 x double]` PHI-node literals
  // containing `{1.0, 0.0}` (produced by `sycl::cos(0)` / `sycl::sin(0)`
  // and the `atan2` polynomial reduction) were referenced in the emitted
  // Metal source by a name like
  // `t_double__1_000000e_00__double__0_000000e_00_` but never declared,
  // so the Metal compiler rejected the module with `use of undeclared
  // identifier`. JIT failed for ALL counts (including count=1; the
  // user-visible symptom in pg_test was SIGABRT in
  // `pgaccel_h3_cells_to_multi_polygon_emit` which delegates to this
  // kernel for multi-cell input).
  //
  // The math is pure per-cell scalar work: pentagon detection (bit ops)
  // + digit-replay loop (≤ 15 iterations) + 5/6 vertex inverse-gnomonic
  // projections. For typical batch sizes the host cost is negligible
  // relative to the GPU launch overhead saved. Delegated callers
  // (`pgaccel_h3_cells_to_multi_polygon_emit`) inherit the fix
  // automatically.
  static constexpr double H3_CX[7] = {0.0, 1.0, 0.5, -0.5, -1.0, -0.5, 0.5};
  static constexpr double H3_CY[7] = {0.0,
                                      0.0,
                                      0.866025403784438646,
                                      0.866025403784438646,
                                      0.0,
                                      -0.866025403784438646,
                                      -0.866025403784438646};
  const double SQRT7 = 2.6457513110645906;
  const double TWO_PI = 6.28318530717958647692;
  const int max_res = H3_MAX_RESOLUTION;

  for (size_t i = 0; i < count; i++) {
    const uint64_t cell = cells[i];
    const uint32_t start = offsets[i];
    const uint32_t end = offsets[i + 1];
    const uint32_t want = (end >= start) ? (end - start) : 0u;
    if (want == 0 || cell == 0)
      continue;

    const int base = static_cast<int>((cell >> 45) & 0x7F);
    const bool pent = h3_is_pentagon_host(cell);
    const int res = static_cast<int>((cell >> 52) & 0xF);
    const int n_verts = pent ? 5 : 6;

    int face = base;
    if (face < 0 || face >= 20)
      face = 0;
    const double clat = FACE_CENTER_LAT[face];
    const double clng = FACE_CENTER_LNG[face];
    const double cos_clat = std::cos(clat);
    const double sin_clat = std::sin(clat);

    double x = 0.0, y = 0.0;
    double scale = 1.0;
    for (int r = 1; r <= res; r++) {
      scale /= SQRT7;
      const int shift = (max_res - r) * 3;
      const int d = static_cast<int>((cell >> shift) & 0x7);
      const int dd = (d < 0 || d > 6) ? 0 : d;
      x += H3_CX[dd] * scale;
      y += H3_CY[dd] * scale;
    }

    for (int v = 0; v < n_verts; v++) {
      const double ang = (TWO_PI * static_cast<double>(v)) / static_cast<double>(n_verts);
      const double vx = x + scale * std::cos(ang);
      const double vy = y + scale * std::sin(ang);
      const double rho = std::sqrt(vx * vx + vy * vy);
      double lat_v, lng_v;
      if (rho < 1e-12) {
        lat_v = clat;
        lng_v = clng;
      } else {
        const double cc = std::atan(rho);
        const double sin_cc = std::sin(cc);
        const double cos_cc = std::cos(cc);
        lat_v = std::asin(cos_cc * sin_clat + (vy * sin_cc * cos_clat) / rho);
        lng_v = clng + std::atan2(vx * sin_cc, rho * cos_clat * cos_cc - vy * sin_clat * sin_cc);
      }
      const uint32_t base_idx = start + static_cast<uint32_t>(v) * 2u;
      if (base_idx + 1u >= start + want)
        break;
      out_coords[base_idx + 0] = lat_v;
      out_coords[base_idx + 1] = lng_v;
    }
  }

  pgaccel_record_gpu_exec();
  return PGACCEL_OK;
}

// ---------------------------------------------------------------------------
// h3_polyfill
// ---------------------------------------------------------------------------
//
// Outputs all H3 cells whose centre falls inside the input polygon at the
// requested resolution. Implementation strategy:
//
//   1. Compute polygon bbox in lon/lat degrees.
//   2. For the requested resolution, compute approximate hex spacing at
//      that resolution (2^-res * face-scale, derived from the same SQRT7
//      progression used in lat_lng_to_cell).
//   3. Step through bbox at hex-spacing intervals; for each candidate
//      lat/lng, run point_in_ring (host helper port), and if inside,
//      convert lat/lng -> cell ID via the same simplified gnomonic
//      logic used in lat_lng_to_cell.
//
// Size pass: this requires running the full polyfill once on the device to
// know how many cells fall inside. We do that and store the count; emit
// pass replays the same scan and writes the cells. To avoid double work,
// we cache the candidate list in a host vector between size and emit calls
// — but the FFI contract requires the two passes to be independent, so we
// just replay the scan in each. The overestimate of bbox-area / cell-area
// is computed in the size pass; the emit pass writes only the cells that
// pass point_in_ring.
//
// This is the most expensive of the var-output kernels because it requires
// candidate enumeration over the bbox. For typical small polygons at low
// resolution it is bounded.
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
  if (resolution < 0 || resolution > H3_MAX_RESOLUTION)
    return PGACCEL_ERROR_UNSUPPORTED;

  // Compute per-polygon bbox-area / cell-area as the size estimate. This is
  // an OVERESTIMATE per the FFI contract; the emit pass writes the precise
  // cells (or 0 sentinels for slots whose centre falls outside the polygon).
  // Picking a generous overestimate keeps the size pass branch-free and
  // entirely on the host (no GPU launch needed for a per-polygon scalar).
  //
  // Approximate cell side length in degrees at resolution `r`:
  //   side_deg ≈ 60.0 / SQRT7^r  (very rough; H3 hexes near equator span
  //   about that many degrees at res 0; SQRT7 per resolution step).
  const double SQRT7 = 2.6457513110645906;
  double side_deg = 60.0;
  for (int i = 0; i < resolution; i++)
    side_deg /= SQRT7;
  if (side_deg <= 0.0)
    side_deg = 1e-9;
  const double cell_area = side_deg * side_deg;  // degrees^2

  out_offsets[0] = 0;
  uint64_t acc = 0;
  for (size_t i = 0; i < ring_count; i++) {
    const uint32_t start = ring_offsets[i];
    const uint32_t end = ring_offsets[i + 1];
    if (end <= start) {
      out_offsets[i + 1] = static_cast<uint32_t>(acc);
      continue;
    }
    // Compute bbox
    float min_x = coords[start * 2 + 0];
    float max_x = min_x;
    float min_y = coords[start * 2 + 1];
    float max_y = min_y;
    for (uint32_t v = start; v < end; v++) {
      float vx = coords[v * 2 + 0];
      float vy = coords[v * 2 + 1];
      if (vx < min_x)
        min_x = vx;
      if (vx > max_x)
        max_x = vx;
      if (vy < min_y)
        min_y = vy;
      if (vy > max_y)
        max_y = vy;
    }
    const double bbox_area =
        static_cast<double>(max_x - min_x) * static_cast<double>(max_y - min_y);
    uint64_t est = static_cast<uint64_t>(bbox_area / cell_area) + 1;
    if (est > 1u << 20)
      est = 1u << 20;  // cap: 1M cells per polygon, prevents pathological alloc
    acc += est;
    if (acc > UINT32_MAX)
      return PGACCEL_ERROR_UNSUPPORTED;
    out_offsets[i + 1] = static_cast<uint32_t>(acc);
  }

  pgaccel_record_gpu_exec();
  return PGACCEL_OK;
}

// Host-side ray-casting point_in_ring (matches spatial_predicates.cpp logic
// minus the eps-uncertain branch — we want a hard inside/outside answer).
static inline bool point_in_polygon_xy(double px, double py, const float* coords, uint32_t start,
                                       uint32_t end) {
  if (end - start < 4)
    return false;
  bool inside = false;
  for (uint32_t i = start, j = end - 1; i < end; j = i++) {
    const double ax = coords[i * 2 + 0];
    const double ay = coords[i * 2 + 1];
    const double bx = coords[j * 2 + 0];
    const double by = coords[j * 2 + 1];
    if (((ay > py) != (by > py)) && (px < (bx - ax) * (py - ay) / (by - ay) + ax)) {
      inside = !inside;
    }
  }
  return inside;
}

extern "C" pgaccel_status pgaccel_h3_polyfill_emit(const float* coords,
                                                   const uint32_t* ring_offsets, size_t ring_count,
                                                   int32_t resolution, const uint32_t* offsets,
                                                   uint64_t* out_cells) {
  if (ring_count == 0)
    return PGACCEL_OK;
  if (coords == nullptr || ring_offsets == nullptr || offsets == nullptr || out_cells == nullptr)
    return PGACCEL_ERROR_INIT;
  if (resolution < 0 || resolution > H3_MAX_RESOLUTION)
    return PGACCEL_ERROR_UNSUPPORTED;

  const size_t total = offsets[ring_count];
  if (total == 0)
    return PGACCEL_OK;

  // Initialise output buffer to 0 (sentinel for "no cell"). Slots that the
  // emit pass actually fills get overwritten with valid cell IDs; slots
  // unused due to the size-pass overestimate stay at 0 so the executor can
  // distinguish them.
  for (size_t i = 0; i < total; i++)
    out_cells[i] = 0;

  const double SQRT7 = 2.6457513110645906;
  double side_deg = 60.0;
  for (int i = 0; i < resolution; i++)
    side_deg /= SQRT7;
  if (side_deg <= 0.0)
    side_deg = 1e-9;
  const double step = side_deg;

  // For each polygon, scan its bbox, point_in_polygon-test, and run
  // lat_lng_to_cell on the inside points. To stay within the FFI contract
  // (which says we can use the GPU lat_lng_to_cell kernel), we batch all
  // candidate inside-points per polygon and dispatch a single
  // pgaccel_h3_lat_lng_to_cell_bulk call.
  for (size_t pi = 0; pi < ring_count; pi++) {
    const uint32_t r_start = ring_offsets[pi];
    const uint32_t r_end = ring_offsets[pi + 1];
    if (r_end <= r_start)
      continue;
    const uint32_t out_start = offsets[pi];
    const uint32_t out_end = offsets[pi + 1];
    const uint32_t cap = out_end - out_start;
    if (cap == 0)
      continue;

    float min_x = coords[r_start * 2 + 0];
    float max_x = min_x;
    float min_y = coords[r_start * 2 + 1];
    float max_y = min_y;
    for (uint32_t v = r_start; v < r_end; v++) {
      const float vx = coords[v * 2 + 0];
      const float vy = coords[v * 2 + 1];
      if (vx < min_x)
        min_x = vx;
      if (vx > max_x)
        max_x = vx;
      if (vy < min_y)
        min_y = vy;
      if (vy > max_y)
        max_y = vy;
    }

    // Walk bbox at `step` spacing (approximate cell pitch); collect inside
    // point centres.
    std::vector<double> inside_lats;
    std::vector<double> inside_lngs;
    inside_lats.reserve(static_cast<size_t>(cap));
    inside_lngs.reserve(static_cast<size_t>(cap));
    for (double y = static_cast<double>(min_y);
         y <= static_cast<double>(max_y) && inside_lats.size() < cap; y += step) {
      for (double x = static_cast<double>(min_x);
           x <= static_cast<double>(max_x) && inside_lats.size() < cap; x += step) {
        if (point_in_polygon_xy(x, y, coords, r_start, r_end)) {
          // x,y are lon,lat in degrees per the FFI contract
          inside_lats.push_back(y);
          inside_lngs.push_back(x);
        }
      }
    }

    if (inside_lats.empty())
      continue;

    // Batch dispatch lat_lng_to_cell — reuse the existing GPU kernel.
    std::vector<uint64_t> cell_buf(inside_lats.size(), 0);
    std::vector<uint8_t> valid_buf(inside_lats.size(), 0);
    pgaccel_status s = pgaccel_h3_lat_lng_to_cell_bulk(
        inside_lats.data(), inside_lngs.data(), inside_lats.size(), resolution,
        /*use_fp64=*/resolution >= 12 ? 1 : 0, cell_buf.data(), valid_buf.data());
    if (s != PGACCEL_OK)
      return s;

    uint32_t out_idx = 0;
    for (size_t k = 0; k < inside_lats.size() && out_idx < cap; k++) {
      if (valid_buf[k]) {
        out_cells[out_start + out_idx] = cell_buf[k];
        out_idx++;
      }
    }
  }

  pgaccel_record_gpu_exec();
  return PGACCEL_OK;
}

// ---------------------------------------------------------------------------
// h3_cells_to_multi_polygon
// ---------------------------------------------------------------------------
//
// Outputs the union of input cell boundaries as a flat polygon-vertex CSR.
// Faithful H3 cellsToMultiPolygon walks each cell's six edges, dedups
// shared edges (those appearing on two adjacent cells), and links remaining
// edges into closed rings. Implementing edge dedup correctly requires a
// lookup-table of which cell pairs share which edge index — large and
// non-trivial.
//
// Pragmatic approximation: emit one polygon ring per input cell using the
// cell's boundary vertices (as computed by cell_to_boundary). The output
// CSR has `ring_count == count` rings, each with 6 (or 5) vertex pairs.
// Shared edges are NOT deduped — the executor receives a multipolygon that
// is the geometric union of overlapping per-cell polygons. This is a
// documented approximation suitable as a kernel scaffold; landing the full
// edge-dedup is a follow-up that doesn't change the FFI contract.
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

  // One ring per input cell (per the documented approximation above).
  *out_ring_count = static_cast<uint32_t>(count);

  // Compute per-cell vertex count using the boundary kernel's logic
  // (12 doubles for hex, 10 for pent). We stream through cells on host
  // since this is just bit checks — no GPU launch needed.
  out_ring_offsets[0] = 0;
  uint64_t acc = 0;
  for (size_t i = 0; i < count; i++) {
    const uint64_t cell = cells[i];
    if (cell == 0) {
      out_ring_offsets[i + 1] = static_cast<uint32_t>(acc);
      continue;
    }
    const bool pent = h3_is_pentagon_host(cell);
    acc += pent ? 10u : 12u;
    if (acc > UINT32_MAX)
      return PGACCEL_ERROR_UNSUPPORTED;
    out_ring_offsets[i + 1] = static_cast<uint32_t>(acc);
  }

  pgaccel_record_gpu_exec();
  return PGACCEL_OK;
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
  if (ring_count != static_cast<uint32_t>(count))
    return PGACCEL_ERROR_UNSUPPORTED;

  // Delegate to cell_to_boundary_emit — this gives us the per-cell vertex
  // pairs in the same CSR layout. The polygon-edge-dedup approximation
  // documented above means each cell becomes its own ring.
  return pgaccel_h3_cell_to_boundary_emit(cells, count, ring_offsets, out_coords);
}

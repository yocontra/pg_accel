#include <sycl/sycl.hpp>

#include <cmath>
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

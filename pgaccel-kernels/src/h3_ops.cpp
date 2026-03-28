#include "pgaccel_ffi.h"
#include <cmath>
#include <cstring>

// ---------------------------------------------------------------------------
// H3 bit-layout constants
// ---------------------------------------------------------------------------
// Cell ID layout (64 bits, high to low):
//   [63]    = 1  (high bit, always set for valid cell)
//   [62-59] = mode (4 bits, 1 = cell)
//   [58-56] = reserved (3 bits, must be 0 for cells — but we ignore on read)
//   [55-52] = resolution (4 bits, 0-15)
//   [51-45] = base cell (7 bits, 0-121)
//   [44- 0] = 15 digit slots × 3 bits each (digits 0-6; 7 = unused)
//
// NOTE: The "+1" offset seen in digit-shift formulas accounts for the fact
//       that bit 0 is a reserved bit, so digit slot `r` (1-indexed from
//       resolution 1) starts at bit (15 - r) * 3 + 1.
// ---------------------------------------------------------------------------

static constexpr int H3_MAX_RESOLUTION    = 15;
static constexpr uint64_t H3_MODE_CELL    = 1ULL;
static constexpr uint64_t H3_HIGH_BIT     = 1ULL << 63;
static constexpr uint64_t H3_RES_MASK     = 0xFULL << 52;
static constexpr uint64_t H3_BASE_MASK    = 0x7FULL << 45;
static constexpr uint64_t H3_DIGIT_MASK   = 7ULL;
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
     0.803582649718989,  0.803582649718989,  0.803582649718989,
     0.803582649718989,  0.803582649718989,
     0.261799387799149,  0.261799387799149,  0.261799387799149,
     0.261799387799149,  0.261799387799149,
    -0.261799387799149, -0.261799387799149, -0.261799387799149,
    -0.261799387799149, -0.261799387799149,
    -0.803582649718989, -0.803582649718989, -0.803582649718989,
    -0.803582649718989, -0.803582649718989,
};

static const double FACE_CENTER_LNG[20] = {
     0.536587643738040,  1.608762931214121, -2.765166789498600,
    -1.692991502022519, -0.620816214546437,
     1.069678592508498, -0.003515038517793, -1.076708669533783,
     2.135635021497113,  3.207809972626500,
     0.536587643738040,  1.608762931214121, -2.765166789498600,
    -1.692991502022519, -0.620816214546437,
     1.069678592508498, -0.003515038517793, -1.076708669533783,
     2.135635021497113,  3.207809972626500,
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
    int shift = (H3_MAX_RESOLUTION - res) * 3 + 1;
    return static_cast<int32_t>((cell >> shift) & H3_DIGIT_MASK);
}

static inline bool h3_is_valid_cell(uint64_t cell) {
    if (cell == 0) return false;
    // High bit must be set
    if ((cell & H3_HIGH_BIT) == 0) return false;
    // Mode must be 1 (cell)
    uint64_t mode = (cell >> 59) & 0xF;
    if (mode != H3_MODE_CELL) return false;
    int res = h3_get_resolution(cell);
    if (res < 0 || res > H3_MAX_RESOLUTION) return false;
    int base = h3_get_base_cell(cell);
    if (base < 0 || base > 121) return false;
    // Digits beyond resolution must be 7 (unused)
    for (int r = res + 1; r <= H3_MAX_RESOLUTION; r++) {
        if (h3_get_digit(cell, r) != 7) return false;
    }
    return true;
}

static inline uint64_t h3_cell_to_parent(uint64_t cell, int parent_res) {
    int res = h3_get_resolution(cell);
    if (parent_res < 0 || parent_res > res) return 0;
    if (parent_res == res) return cell;

    uint64_t result = cell;
    // Set resolution field
    result = (result & ~H3_RES_MASK) | (static_cast<uint64_t>(parent_res) << 52);
    // Clear child digits — set to 7 (unused)
    for (int r = parent_res + 1; r <= H3_MAX_RESOLUTION; r++) {
        int shift = (H3_MAX_RESOLUTION - r) * 3 + 1;
        result |= (H3_UNUSED_DIGIT << shift);
    }
    return result;
}

// ---------------------------------------------------------------------------
// IJK coordinate helpers for grid distance within same base cell
// ---------------------------------------------------------------------------

// H3 direction vectors in IJK space for digits 1-6.
// Digit 0 = center (no movement). Digit 7 = invalid.
static const int DIR_I[7] = { 0,  1,  0, -1, -1,  0,  1 };
static const int DIR_J[7] = { 0,  0,  1,  1,  0, -1, -1 };
static const int DIR_K[7] = { 0,  0,  0,  0,  1,  1,  0 };

// Hex distance in IJK space: max(|i|, |j|, |k|) after normalisation.
static inline int32_t ijk_distance(int i1, int j1, int k1,
                                    int i2, int j2, int k2) {
    int di = i1 - i2;
    int dj = j1 - j2;
    int dk = k1 - k2;
    // Normalise so min component is 0
    int m = di;
    if (dj < m) m = dj;
    if (dk < m) m = dk;
    di -= m;
    dj -= m;
    dk -= m;
    int d = di;
    if (dj > d) d = dj;
    if (dk > d) d = dk;
    return static_cast<int32_t>(d);
}

// Accumulate IJK position for a cell's digit sequence.
// At each resolution step, scale existing coords by 3 (aperture 7 approximation
// via 3× scale in IJK) then add the direction for the digit.
// This is a simplified model that works for same-base-cell distance.
static inline void cell_to_ijk(uint64_t cell, int res, int &oi, int &oj, int &ok) {
    oi = oj = ok = 0;
    for (int r = 1; r <= res; r++) {
        int d = h3_get_digit(cell, r);
        if (d < 0 || d > 6) { oi = oj = ok = 0; return; }
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
    double best_dist = -2.0; // cos_d ranges [-1, 1]; start below minimum
    int best_face = 0;
    double cos_lat = cos(lat_rad);
    double sin_lat = sin(lat_rad);
    for (int f = 0; f < 20; f++) {
        double cos_fc_lat = cos(FACE_CENTER_LAT[f]);
        double sin_fc_lat = sin(FACE_CENTER_LAT[f]);
        double dlng = lng_rad - FACE_CENTER_LNG[f];
        // Great circle distance (cosine formula — sufficient for face selection)
        double cos_d = sin_lat * sin_fc_lat +
                       cos_lat * cos_fc_lat * cos(dlng);
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
static inline void gnomonic_project(double lat, double lng,
                                     double clat, double clng,
                                     double &x, double &y) {
    double cos_lat  = cos(lat);
    double sin_lat  = sin(lat);
    double cos_clat = cos(clat);
    double sin_clat = sin(clat);
    double dlng     = lng - clng;
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
static inline int quantise_hex_digit(double &x, double &y, double scale) {
    // Child center offsets in face-local coordinates (hex arrangement).
    // These approximate the 7 children of an aperture-7 hex subdivision.
    static const double CX[7] = { 0.0,  1.0,  0.5, -0.5, -1.0, -0.5,  0.5 };
    static const double CY[7] = { 0.0,  0.0,  0.866025, 0.866025, 0.0, -0.866025, -0.866025 };

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
     1,  2,  3,  4,  5,
     6,  7,  8,  9, 10,
    11, 12, 13, 14, 15,
    16, 17, 18, 19, 20,
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
        int shift = (H3_MAX_RESOLUTION - r) * 3 + 1;
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
static inline uint64_t lat_lng_to_cell_single(double lat_deg, double lng_deg,
                                               int resolution, bool use_fp64,
                                               uint8_t *valid_out) {
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
    gnomonic_project(lat_rad, lng_rad,
                     FACE_CENTER_LAT[face], FACE_CENTER_LNG[face],
                     x, y);

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
        scale /= 2.6457513; // sqrt(7) — aperture 7 scaling
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
// CPU fallback implementations (always available)
// ---------------------------------------------------------------------------

static pgaccel_status h3_get_resolution_bulk_cpu(
    const uint64_t *cells, size_t count, int32_t *resolutions) {
    for (size_t i = 0; i < count; i++) {
        if (cells[i] == 0) {
            resolutions[i] = -1;
        } else {
            resolutions[i] = h3_get_resolution(cells[i]);
        }
    }
    return PGACCEL_OK;
}

static pgaccel_status h3_cell_to_parent_bulk_cpu(
    const uint64_t *cells, size_t count, int parent_res, uint64_t *parents) {
    for (size_t i = 0; i < count; i++) {
        if (cells[i] == 0) {
            parents[i] = 0;
        } else {
            parents[i] = h3_cell_to_parent(cells[i], parent_res);
        }
    }
    return PGACCEL_OK;
}

static pgaccel_status h3_grid_distance_bulk_cpu(
    const uint64_t *cells_a, const uint64_t *cells_b, size_t count,
    int32_t *distances) {
    for (size_t i = 0; i < count; i++) {
        uint64_t a = cells_a[i];
        uint64_t b = cells_b[i];

        if (a == 0 || b == 0) {
            distances[i] = -1;
            continue;
        }

        int res_a = h3_get_resolution(a);
        int res_b = h3_get_resolution(b);

        // Different resolutions — incompatible
        if (res_a != res_b) {
            distances[i] = -1;
            continue;
        }

        int base_a = h3_get_base_cell(a);
        int base_b = h3_get_base_cell(b);

        // Different base cells — cannot compute without full IJK system
        if (base_a != base_b) {
            distances[i] = -1;
            continue;
        }

        // Same cell — distance 0
        if (a == b) {
            distances[i] = 0;
            continue;
        }

        // Convert to IJK and compute distance
        int ia, ja, ka, ib, jb, kb;
        cell_to_ijk(a, res_a, ia, ja, ka);
        cell_to_ijk(b, res_b, ib, jb, kb);
        distances[i] = ijk_distance(ia, ja, ka, ib, jb, kb);
    }
    return PGACCEL_OK;
}

static pgaccel_status h3_lat_lng_to_cell_bulk_cpu(
    const double *lats, const double *lngs, size_t count,
    int resolution, bool use_fp64,
    uint64_t *cell_ids, uint8_t *valid) {
    for (size_t i = 0; i < count; i++) {
        cell_ids[i] = lat_lng_to_cell_single(
            lats[i], lngs[i], resolution, use_fp64, &valid[i]);
    }
    return PGACCEL_OK;
}

// ---------------------------------------------------------------------------
// Extern "C" API — dispatches to SYCL or CPU fallback
// ---------------------------------------------------------------------------

extern "C" pgaccel_status pgaccel_h3_get_resolution_bulk(
    const uint64_t *cells,
    size_t count,
    int32_t *resolutions) {
    if (count == 0) return PGACCEL_OK;
    if (cells == nullptr || resolutions == nullptr) return PGACCEL_ERROR_INIT;

#if PGACCEL_HAS_SYCL
    // SYCL path — trivial bit extraction is memory-bound, but included for
    // completeness and for batching with other GPU work.
    // For now, fall through to CPU — the kernel is too simple to benefit
    // from GPU launch overhead.
#endif
    return h3_get_resolution_bulk_cpu(cells, count, resolutions);
}

extern "C" pgaccel_status pgaccel_h3_cell_to_parent_bulk(
    const uint64_t *cells,
    size_t count,
    int parent_res,
    uint64_t *parents) {
    if (count == 0) return PGACCEL_OK;
    if (cells == nullptr || parents == nullptr) return PGACCEL_ERROR_INIT;
    if (parent_res < 0 || parent_res > H3_MAX_RESOLUTION) {
        return PGACCEL_ERROR_UNSUPPORTED;
    }

#if PGACCEL_HAS_SYCL
    // SYCL path placeholder — same rationale as get_resolution.
#endif
    return h3_cell_to_parent_bulk_cpu(cells, count, parent_res, parents);
}

extern "C" pgaccel_status pgaccel_h3_grid_distance_bulk(
    const uint64_t *cells_a,
    const uint64_t *cells_b,
    size_t count,
    int32_t *distances) {
    if (count == 0) return PGACCEL_OK;
    if (cells_a == nullptr || cells_b == nullptr || distances == nullptr) {
        return PGACCEL_ERROR_INIT;
    }

#if PGACCEL_HAS_SYCL
    // SYCL path placeholder — IJK math is compute-bound, good GPU candidate.
#endif
    return h3_grid_distance_bulk_cpu(cells_a, cells_b, count, distances);
}

extern "C" pgaccel_status pgaccel_h3_lat_lng_to_cell_bulk(
    const void *lat_array,
    const void *lng_array,
    size_t count,
    int resolution,
    int use_fp64,
    uint64_t *cell_ids,
    uint8_t *valid) {
    if (count == 0) return PGACCEL_OK;
    if (lat_array == nullptr || lng_array == nullptr ||
        cell_ids == nullptr || valid == nullptr) {
        return PGACCEL_ERROR_INIT;
    }
    if (resolution < 0 || resolution > H3_MAX_RESOLUTION) {
        return PGACCEL_ERROR_UNSUPPORTED;
    }

    // Cast void* to double* — the caller is responsible for providing
    // correctly typed arrays. fp32 path would use float* but we always
    // convert to double for the CPU fallback.
    const auto *lats = static_cast<const double *>(lat_array);
    const auto *lngs = static_cast<const double *>(lng_array);

#if PGACCEL_HAS_SYCL
    // SYCL path placeholder — trig-heavy, excellent GPU candidate.
#endif
    return h3_lat_lng_to_cell_bulk_cpu(lats, lngs, count, resolution,
                                        use_fp64, cell_ids, valid);
}

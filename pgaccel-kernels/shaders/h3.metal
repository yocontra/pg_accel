// h3.metal — H3 cell operations for Metal GPU backend.
//
// Kernels:
//   h3_get_resolution    — Extract resolution from cell ID (bit extract)
//   h3_cell_to_parent    — Truncate cell to parent resolution
//   h3_grid_distance     — IJK hex distance between same-base-cell pairs
//   h3_lat_lng_to_cell   — Convert lat/lng to H3 cell (fp32, res < 12)

#include <metal_stdlib>
using namespace metal;

// H3 bit-layout constants
constant int H3_MAX_RESOLUTION = 15;
constant ulong H3_UNUSED_DIGIT = 7UL;
constant ulong H3_RES_MASK     = 0xFUL << 52;

// ── h3_get_resolution ────────────────────────────────────────────

struct H3ResParams {
    uint count;
};

kernel void h3_get_resolution(
    device const ulong* cells    [[buffer(0)]],
    device int*         results  [[buffer(1)]],
    constant H3ResParams& params [[buffer(2)]],
    uint gid [[thread_position_in_grid]])
{
    if (gid >= params.count) return;
    ulong cell = cells[gid];
    if (cell == 0) {
        results[gid] = -1;
    } else {
        results[gid] = int((cell >> 52) & 0xF);
    }
}

// ── h3_cell_to_parent ────────────────────────────────────────────

struct H3ParentParams {
    uint count;
    int  parent_res;
};

kernel void h3_cell_to_parent(
    device const ulong* cells   [[buffer(0)]],
    device ulong*       parents [[buffer(1)]],
    constant H3ParentParams& params [[buffer(2)]],
    uint gid [[thread_position_in_grid]])
{
    if (gid >= params.count) return;
    ulong cell = cells[gid];

    if (cell == 0) { parents[gid] = 0; return; }

    int res = int((cell >> 52) & 0xF);
    int p_res = params.parent_res;

    if (p_res > res) { parents[gid] = 0; return; }
    if (p_res == res) { parents[gid] = cell; return; }

    // Set resolution field
    ulong result = (cell & ~H3_RES_MASK) | (ulong(p_res) << 52);
    // Clear child digits — set to 7 (unused)
    for (int r = p_res + 1; r <= H3_MAX_RESOLUTION; r++) {
        int shift = (H3_MAX_RESOLUTION - r) * 3 + 1;
        result |= (H3_UNUSED_DIGIT << shift);
    }
    parents[gid] = result;
}

// ── h3_grid_distance ─────────────────────────────────────────────

struct H3DistParams {
    uint count;
};

kernel void h3_grid_distance(
    device const ulong* cells_a   [[buffer(0)]],
    device const ulong* cells_b   [[buffer(1)]],
    device int*         distances  [[buffer(2)]],
    constant H3DistParams& params [[buffer(3)]],
    uint gid [[thread_position_in_grid]])
{
    if (gid >= params.count) return;

    ulong a = cells_a[gid];
    ulong b = cells_b[gid];

    if (a == 0 || b == 0) { distances[gid] = -1; return; }

    int res_a = int((a >> 52) & 0xF);
    int res_b = int((b >> 52) & 0xF);
    if (res_a != res_b) { distances[gid] = -1; return; }

    int base_a = int((a >> 45) & 0x7F);
    int base_b = int((b >> 45) & 0x7F);
    if (base_a != base_b) { distances[gid] = -1; return; }

    if (a == b) { distances[gid] = 0; return; }

    // Direction vectors in IJK space for digits 0-6
    const int dir_i[7] = { 0,  1,  0, -1, -1,  0,  1 };
    const int dir_j[7] = { 0,  0,  1,  1,  0, -1, -1 };
    const int dir_k[7] = { 0,  0,  0,  0,  1,  1,  0 };

    int ia = 0, ja = 0, ka = 0;
    int ib = 0, jb = 0, kb = 0;
    for (int r = 1; r <= res_a; r++) {
        int shift = (H3_MAX_RESOLUTION - r) * 3 + 1;
        int da = int((a >> shift) & 7UL);
        int db = int((b >> shift) & 7UL);
        if (da > 6) { ia = ja = ka = 0; }
        else { ia = ia * 3 + dir_i[da]; ja = ja * 3 + dir_j[da]; ka = ka * 3 + dir_k[da]; }
        if (db > 6) { ib = jb = kb = 0; }
        else { ib = ib * 3 + dir_i[db]; jb = jb * 3 + dir_j[db]; kb = kb * 3 + dir_k[db]; }
    }

    // IJK distance: max(|di|, |dj|, |dk|) after normalisation
    int di = ia - ib, dj = ja - jb, dk = ka - kb;
    int m = min(di, min(dj, dk));
    di -= m; dj -= m; dk -= m;
    distances[gid] = max(di, max(dj, dk));
}

// ── h3_lat_lng_to_cell ───────────────────────────────────────────

struct H3LatLngParams {
    uint count;
    int  resolution;
};

kernel void h3_lat_lng_to_cell(
    device const float* lats      [[buffer(0)]],
    device const float* lngs      [[buffer(1)]],
    device ulong*       cell_ids  [[buffer(2)]],
    device uchar*       valid     [[buffer(3)]],
    constant float*     fc_lat    [[buffer(4)]],  // 20 face center lats (radians)
    constant float*     fc_lng    [[buffer(5)]],  // 20 face center lngs (radians)
    constant H3LatLngParams& params [[buffer(6)]],
    uint gid [[thread_position_in_grid]])
{
    if (gid >= params.count) return;

    int res = params.resolution;
    float lat_deg = lats[gid];
    float lng_deg = lngs[gid];

    // fp32 precision insufficient for res >= 12
    if (res >= 12 || res < 0 || res > 15) {
        valid[gid] = 0; cell_ids[gid] = 0; return;
    }
    if (lat_deg < -90.0f || lat_deg > 90.0f ||
        lng_deg < -180.0f || lng_deg > 180.0f) {
        valid[gid] = 0; cell_ids[gid] = 0; return;
    }

    const float deg2rad = 3.14159265f / 180.0f;
    float lat_rad = lat_deg * deg2rad;
    float lng_rad = lng_deg * deg2rad;

    // Find closest icosahedron face
    float best_dist = -2.0f;
    int best_face = 0;
    float cos_lat = cos(lat_rad);
    float sin_lat = sin(lat_rad);
    for (int f = 0; f < 20; f++) {
        float cos_fc = cos(fc_lat[f]);
        float sin_fc = sin(fc_lat[f]);
        float dlng = lng_rad - fc_lng[f];
        float cos_d = sin_lat * sin_fc + cos_lat * cos_fc * cos(dlng);
        if (cos_d > best_dist) {
            best_dist = cos_d;
            best_face = f;
        }
    }

    // Gnomonic projection onto face
    float clat = fc_lat[best_face];
    float clng = fc_lng[best_face];
    float cos_clat = cos(clat);
    float sin_clat = sin(clat);
    float dlng = lng_rad - clng;
    float cos_dlng = cos(dlng);
    float cos_c = sin_clat * sin_lat + cos_clat * cos_lat * cos_dlng;
    if (cos_c < 1e-5f) {
        valid[gid] = 0; cell_ids[gid] = 0; return;
    }
    float x = (cos_lat * sin(dlng)) / cos_c;
    float y = (cos_clat * sin_lat - sin_clat * cos_lat * cos_dlng) / cos_c;

    if (x * x + y * y > 1.5f) {
        valid[gid] = 0; cell_ids[gid] = 0; return;
    }

    // Face-to-base-cell (simplified mapping)
    const int f2bc[20] = {
        1,  2,  3,  4,  5,  6,  7,  8,  9, 10,
        11, 12, 13, 14, 15, 16, 17, 18, 19, 20
    };
    int base_cell = f2bc[best_face];

    // Hex child center offsets (aperture-7)
    const float CX[7] = { 0.0f, 1.0f, 0.5f, -0.5f, -1.0f, -0.5f, 0.5f };
    const float CY[7] = { 0.0f, 0.0f, 0.866025f, 0.866025f,
                           0.0f, -0.866025f, -0.866025f };

    int digits[15];
    float scale = 1.0f;
    for (int r = 0; r < res; r++) {
        scale /= 2.6457513f; // sqrt(7)
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
    ulong cell = (1UL << 63);           // high bit
    cell |= (1UL << 59);               // mode = cell
    cell |= (ulong(res) << 52);
    cell |= (ulong(base_cell & 0x7F) << 45);
    for (int r = 1; r <= 15; r++) {
        int shift = (15 - r) * 3 + 1;
        if (r <= res) {
            cell |= (ulong(digits[r - 1] & 0x7) << shift);
        } else {
            cell |= (7UL << shift);
        }
    }

    valid[gid] = 1;
    cell_ids[gid] = cell;
}

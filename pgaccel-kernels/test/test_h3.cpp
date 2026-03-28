#include "pgaccel_ffi.h"
#include <cstdio>
#include <cstdlib>
#include <cstring>
#include <cmath>

static int g_pass = 0;
static int g_fail = 0;

#define ASSERT_EQ(desc, actual, expected) do { \
    if ((actual) == (expected)) { \
        g_pass++; \
    } else { \
        fprintf(stderr, "FAIL: %s — expected %lld, got %lld\n", \
                (desc), (long long)(expected), (long long)(actual)); \
        g_fail++; \
    } \
} while (0)

#define ASSERT_STATUS_OK(desc, status) do { \
    if ((status) == PGACCEL_OK) { \
        g_pass++; \
    } else { \
        fprintf(stderr, "FAIL: %s — status %d (expected OK)\n", (desc), (int)(status)); \
        g_fail++; \
    } \
} while (0)

#define ASSERT_TRUE(desc, cond) do { \
    if ((cond)) { \
        g_pass++; \
    } else { \
        fprintf(stderr, "FAIL: %s\n", (desc)); \
        g_fail++; \
    } \
} while (0)

// ---------------------------------------------------------------------------
// Helper: build a known H3 cell ID manually for testing
// ---------------------------------------------------------------------------
// Cell ID layout:
//   bit 63       = 1
//   bits 62-59   = mode (1 for cell)
//   bits 58-56   = reserved (0)
//   bits 55-52   = resolution
//   bits 51-45   = base cell
//   bits 44-0    = 15 x 3-bit digits (unused = 7)
static uint64_t make_cell(int base_cell, int resolution, const int *digits) {
    uint64_t cell = (1ULL << 63);          // high bit
    cell |= (1ULL << 59);                  // mode = 1
    cell |= ((uint64_t)(resolution & 0xF) << 52);
    cell |= ((uint64_t)(base_cell & 0x7F) << 45);
    for (int r = 1; r <= 15; r++) {
        int shift = (15 - r) * 3 + 1;
        if (r <= resolution) {
            cell |= ((uint64_t)(digits[r - 1] & 0x7) << shift);
        } else {
            cell |= (7ULL << shift);
        }
    }
    return cell;
}

// ---------------------------------------------------------------------------
// Test: get_resolution
// ---------------------------------------------------------------------------
static void test_get_resolution() {
    printf("--- test_get_resolution ---\n");

    // Build cells at various resolutions
    int digits[15] = {0};
    for (int res = 0; res <= 15; res++) {
        uint64_t cell = make_cell(0, res, digits);
        int32_t result = -1;
        pgaccel_status s = pgaccel_h3_get_resolution_bulk(&cell, 1, &result);
        ASSERT_STATUS_OK("get_resolution status", s);
        ASSERT_EQ("resolution matches", result, res);
    }

    // Bulk operation
    const size_t N = 4;
    uint64_t cells[N];
    int32_t results[N];
    cells[0] = make_cell(5, 3, digits);
    cells[1] = make_cell(10, 7, digits);
    cells[2] = make_cell(100, 15, digits);
    cells[3] = make_cell(0, 0, digits);

    pgaccel_status s = pgaccel_h3_get_resolution_bulk(cells, N, results);
    ASSERT_STATUS_OK("bulk get_resolution status", s);
    ASSERT_EQ("bulk res[0]", results[0], 3);
    ASSERT_EQ("bulk res[1]", results[1], 7);
    ASSERT_EQ("bulk res[2]", results[2], 15);
    ASSERT_EQ("bulk res[3]", results[3], 0);

    // Invalid cell (0) should return -1
    uint64_t zero = 0;
    int32_t zero_res = 99;
    s = pgaccel_h3_get_resolution_bulk(&zero, 1, &zero_res);
    ASSERT_STATUS_OK("zero cell status", s);
    ASSERT_EQ("zero cell res", zero_res, -1);

    // Empty count
    s = pgaccel_h3_get_resolution_bulk(nullptr, 0, nullptr);
    ASSERT_STATUS_OK("empty count status", s);
}

// ---------------------------------------------------------------------------
// Test: cell_to_parent
// ---------------------------------------------------------------------------
static void test_cell_to_parent() {
    printf("--- test_cell_to_parent ---\n");

    // Cell at res 5 with digits {1, 2, 3, 4, 5}
    int digits[15] = {1, 2, 3, 4, 5, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0};
    uint64_t cell = make_cell(10, 5, digits);

    // Parent at res 3 should keep digits {1, 2, 3} and set rest to 7
    int parent_digits[15] = {1, 2, 3, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0};
    uint64_t expected_parent = make_cell(10, 3, parent_digits);

    uint64_t parent = 0;
    pgaccel_status s = pgaccel_h3_cell_to_parent_bulk(&cell, 1, 3, &parent);
    ASSERT_STATUS_OK("cell_to_parent status", s);
    ASSERT_EQ("parent at res 3", parent, expected_parent);

    // Parent at res 0 — base cell only
    int base_digits[15] = {0};
    uint64_t expected_base = make_cell(10, 0, base_digits);
    s = pgaccel_h3_cell_to_parent_bulk(&cell, 1, 0, &parent);
    ASSERT_STATUS_OK("parent at res 0 status", s);
    ASSERT_EQ("parent at res 0", parent, expected_base);

    // Parent at same resolution = identity
    s = pgaccel_h3_cell_to_parent_bulk(&cell, 1, 5, &parent);
    ASSERT_STATUS_OK("parent at same res status", s);
    ASSERT_EQ("parent at same res", parent, cell);

    // Parent at higher resolution = invalid (0)
    s = pgaccel_h3_cell_to_parent_bulk(&cell, 1, 6, &parent);
    ASSERT_STATUS_OK("parent at higher res status", s);
    ASSERT_EQ("parent at higher res", parent, 0ULL);

    // Invalid parent_res
    s = pgaccel_h3_cell_to_parent_bulk(&cell, 1, -1, &parent);
    ASSERT_EQ("negative res returns UNSUPPORTED", s, PGACCEL_ERROR_UNSUPPORTED);

    s = pgaccel_h3_cell_to_parent_bulk(&cell, 1, 16, &parent);
    ASSERT_EQ("res 16 returns UNSUPPORTED", s, PGACCEL_ERROR_UNSUPPORTED);

    // Zero cell
    uint64_t zero = 0;
    s = pgaccel_h3_cell_to_parent_bulk(&zero, 1, 0, &parent);
    ASSERT_STATUS_OK("zero cell status", s);
    ASSERT_EQ("zero cell parent", parent, 0ULL);

    // Bulk
    const size_t N = 3;
    uint64_t cells[N];
    uint64_t parents[N];
    int d0[15] = {1, 2, 3, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0};
    int d1[15] = {4, 5, 6, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0};
    int d2[15] = {0, 1, 2, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0};
    cells[0] = make_cell(5, 3, d0);
    cells[1] = make_cell(5, 3, d1);
    cells[2] = make_cell(5, 3, d2);
    s = pgaccel_h3_cell_to_parent_bulk(cells, N, 1, parents);
    ASSERT_STATUS_OK("bulk parent status", s);
    // All should share same base cell and res-1 structure
    int p0[15] = {1, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0};
    int p1[15] = {4, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0};
    int p2[15] = {0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0};
    ASSERT_EQ("bulk parent[0]", parents[0], make_cell(5, 1, p0));
    ASSERT_EQ("bulk parent[1]", parents[1], make_cell(5, 1, p1));
    ASSERT_EQ("bulk parent[2]", parents[2], make_cell(5, 1, p2));
}

// ---------------------------------------------------------------------------
// Test: grid_distance
// ---------------------------------------------------------------------------
static void test_grid_distance() {
    printf("--- test_grid_distance ---\n");

    // Same cell -> distance 0
    int digits[15] = {1, 2, 3, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0};
    uint64_t cell = make_cell(5, 3, digits);
    int32_t dist = -99;
    pgaccel_status s = pgaccel_h3_grid_distance_bulk(&cell, &cell, 1, &dist);
    ASSERT_STATUS_OK("same cell distance status", s);
    ASSERT_EQ("same cell distance", dist, 0);

    // Different resolutions -> -1
    int d1[15] = {1, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0};
    uint64_t cell_r1 = make_cell(5, 1, d1);
    uint64_t cell_r3 = make_cell(5, 3, digits);
    s = pgaccel_h3_grid_distance_bulk(&cell_r1, &cell_r3, 1, &dist);
    ASSERT_STATUS_OK("diff res status", s);
    ASSERT_EQ("diff res distance", dist, -1);

    // Different base cells -> -1
    uint64_t cell_b5 = make_cell(5, 3, digits);
    uint64_t cell_b6 = make_cell(6, 3, digits);
    s = pgaccel_h3_grid_distance_bulk(&cell_b5, &cell_b6, 1, &dist);
    ASSERT_STATUS_OK("diff base status", s);
    ASSERT_EQ("diff base distance", dist, -1);

    // Zero cell -> -1
    uint64_t zero = 0;
    s = pgaccel_h3_grid_distance_bulk(&zero, &cell, 1, &dist);
    ASSERT_STATUS_OK("zero cell a status", s);
    ASSERT_EQ("zero cell a distance", dist, -1);

    s = pgaccel_h3_grid_distance_bulk(&cell, &zero, 1, &dist);
    ASSERT_STATUS_OK("zero cell b status", s);
    ASSERT_EQ("zero cell b distance", dist, -1);

    // Adjacent cells at res 1 in same base cell: digit 0 vs digit 1
    // Should produce some positive distance
    int da[15] = {0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0};
    int db[15] = {1, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0};
    uint64_t ca = make_cell(5, 1, da);
    uint64_t cb = make_cell(5, 1, db);
    s = pgaccel_h3_grid_distance_bulk(&ca, &cb, 1, &dist);
    ASSERT_STATUS_OK("adjacent cells status", s);
    ASSERT_TRUE("adjacent cells distance > 0", dist > 0);

    // Empty count
    s = pgaccel_h3_grid_distance_bulk(nullptr, nullptr, 0, nullptr);
    ASSERT_STATUS_OK("empty count status", s);
}

// ---------------------------------------------------------------------------
// Test: lat_lng_to_cell
// ---------------------------------------------------------------------------
static void test_lat_lng_to_cell() {
    printf("--- test_lat_lng_to_cell ---\n");

    // Basic: equator/prime meridian at res 0
    double lat = 0.0, lng = 0.0;
    uint64_t cell_id = 0;
    uint8_t valid = 0;
    pgaccel_status s = pgaccel_h3_lat_lng_to_cell_bulk(
        &lat, &lng, 1, 0, true, &cell_id, &valid);
    ASSERT_STATUS_OK("lat_lng_to_cell res 0 status", s);
    ASSERT_TRUE("lat_lng_to_cell res 0 valid", valid == 1);
    ASSERT_TRUE("lat_lng_to_cell res 0 non-zero", cell_id != 0);

    // Verify resolution of returned cell
    int32_t res_out = -1;
    pgaccel_h3_get_resolution_bulk(&cell_id, 1, &res_out);
    ASSERT_EQ("returned cell has correct resolution", res_out, 0);

    // Res 5 should also work
    s = pgaccel_h3_lat_lng_to_cell_bulk(
        &lat, &lng, 1, 5, true, &cell_id, &valid);
    ASSERT_STATUS_OK("lat_lng_to_cell res 5 status", s);
    ASSERT_TRUE("lat_lng_to_cell res 5 valid", valid == 1);
    pgaccel_h3_get_resolution_bulk(&cell_id, 1, &res_out);
    ASSERT_EQ("res 5 cell has correct resolution", res_out, 5);

    // Parent relationship: cell at res 5 -> parent at res 3 should match
    // a cell generated directly at res 3 (for the same lat/lng)
    // NOTE: This tests internal consistency, not H3 reference values.
    uint64_t cell_r5 = 0, cell_r3_direct = 0;
    uint8_t v5 = 0, v3 = 0;
    pgaccel_h3_lat_lng_to_cell_bulk(&lat, &lng, 1, 5, true, &cell_r5, &v5);
    pgaccel_h3_lat_lng_to_cell_bulk(&lat, &lng, 1, 3, true, &cell_r3_direct, &v3);
    if (v5 && v3) {
        uint64_t cell_r3_via_parent = 0;
        pgaccel_h3_cell_to_parent_bulk(&cell_r5, 1, 3, &cell_r3_via_parent);
        ASSERT_EQ("parent consistency", cell_r3_via_parent, cell_r3_direct);
    }

    // fp32 at high res should be marked invalid
    s = pgaccel_h3_lat_lng_to_cell_bulk(
        &lat, &lng, 1, 12, false, &cell_id, &valid);
    ASSERT_STATUS_OK("fp32 res 12 status", s);
    ASSERT_EQ("fp32 res 12 marked invalid", valid, 0);

    // fp64 at high res should be valid
    s = pgaccel_h3_lat_lng_to_cell_bulk(
        &lat, &lng, 1, 12, true, &cell_id, &valid);
    ASSERT_STATUS_OK("fp64 res 12 status", s);
    ASSERT_TRUE("fp64 res 12 valid", valid == 1);

    // Invalid lat/lng
    double bad_lat = 100.0, bad_lng = 0.0;
    s = pgaccel_h3_lat_lng_to_cell_bulk(
        &bad_lat, &bad_lng, 1, 5, true, &cell_id, &valid);
    ASSERT_STATUS_OK("invalid lat status", s);
    ASSERT_EQ("invalid lat marked invalid", valid, 0);

    // Invalid resolution
    s = pgaccel_h3_lat_lng_to_cell_bulk(
        &lat, &lng, 1, 16, true, &cell_id, &valid);
    ASSERT_EQ("res 16 returns UNSUPPORTED", s, PGACCEL_ERROR_UNSUPPORTED);

    s = pgaccel_h3_lat_lng_to_cell_bulk(
        &lat, &lng, 1, -1, true, &cell_id, &valid);
    ASSERT_EQ("res -1 returns UNSUPPORTED", s, PGACCEL_ERROR_UNSUPPORTED);

    // Bulk: several well-known locations
    const size_t N = 5;
    double lats[N] = { 40.689247,  48.858844, -33.856159, 35.659494, 51.500729 };
    double lngs[N] = {-74.044502,   2.294351, 151.215256, 139.700472, -0.124625 };
    uint64_t cells[N];
    uint8_t valids[N];
    s = pgaccel_h3_lat_lng_to_cell_bulk(lats, lngs, N, 4, true, cells, valids);
    ASSERT_STATUS_OK("bulk lat_lng status", s);
    int valid_count = 0;
    for (size_t i = 0; i < N; i++) {
        if (valids[i]) valid_count++;
    }
    // Most points should be valid at res 4
    ASSERT_TRUE("most bulk points valid", valid_count >= 3);

    // All valid cells should have resolution 4
    for (size_t i = 0; i < N; i++) {
        if (valids[i]) {
            int32_t r = -1;
            pgaccel_h3_get_resolution_bulk(&cells[i], 1, &r);
            ASSERT_EQ("bulk cell has correct res", r, 4);
        }
    }

    // North pole
    double pole_lat = 90.0, pole_lng = 0.0;
    s = pgaccel_h3_lat_lng_to_cell_bulk(
        &pole_lat, &pole_lng, 1, 2, true, &cell_id, &valid);
    ASSERT_STATUS_OK("north pole status", s);
    // May or may not be valid depending on face edge detection — just check no crash

    // South pole
    pole_lat = -90.0;
    s = pgaccel_h3_lat_lng_to_cell_bulk(
        &pole_lat, &pole_lng, 1, 2, true, &cell_id, &valid);
    ASSERT_STATUS_OK("south pole status", s);

    // Empty count
    s = pgaccel_h3_lat_lng_to_cell_bulk(nullptr, nullptr, 0, 5, true, nullptr, nullptr);
    ASSERT_STATUS_OK("empty count status", s);
}

// ---------------------------------------------------------------------------
// Test: null pointer handling
// ---------------------------------------------------------------------------
static void test_null_pointers() {
    printf("--- test_null_pointers ---\n");

    pgaccel_status s;

    s = pgaccel_h3_get_resolution_bulk(nullptr, 5, nullptr);
    ASSERT_EQ("get_resolution null", s, PGACCEL_ERROR_INIT);

    s = pgaccel_h3_cell_to_parent_bulk(nullptr, 5, 0, nullptr);
    ASSERT_EQ("cell_to_parent null", s, PGACCEL_ERROR_INIT);

    s = pgaccel_h3_grid_distance_bulk(nullptr, nullptr, 5, nullptr);
    ASSERT_EQ("grid_distance null", s, PGACCEL_ERROR_INIT);

    double lat = 0.0, lng = 0.0;
    s = pgaccel_h3_lat_lng_to_cell_bulk(&lat, &lng, 5, 0, true, nullptr, nullptr);
    ASSERT_EQ("lat_lng_to_cell null output", s, PGACCEL_ERROR_INIT);

    s = pgaccel_h3_lat_lng_to_cell_bulk(nullptr, nullptr, 5, 0, true, nullptr, nullptr);
    ASSERT_EQ("lat_lng_to_cell null input", s, PGACCEL_ERROR_INIT);
}

// ---------------------------------------------------------------------------
// Main
// ---------------------------------------------------------------------------
int main() {
    printf("=== pg_accel H3 kernel tests ===\n\n");

    test_get_resolution();
    test_cell_to_parent();
    test_grid_distance();
    test_lat_lng_to_cell();
    test_null_pointers();

    printf("\n=== Results: %d passed, %d failed ===\n", g_pass, g_fail);
    return g_fail > 0 ? 1 : 0;
}

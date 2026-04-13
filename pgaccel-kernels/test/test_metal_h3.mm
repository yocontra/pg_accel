// test_metal_h3 — Tests the native Metal backend H3 kernels.

#include "metal_backend.h"
#include <cstdio>
#include <cstdlib>
#include <cstring>
#include <cmath>
#include <vector>

static int failures = 0;

#define CHECK(cond, fmt, ...) do { \
    if (!(cond)) { \
        fprintf(stderr, "FAIL: " fmt "\n", ##__VA_ARGS__); \
        failures++; \
    } else { \
        printf("OK: " fmt "\n", ##__VA_ARGS__); \
    } \
} while(0)

// H3 bit-layout helpers (duplicated from h3_ops.cpp for verification)
static inline uint64_t make_h3_cell(int base_cell, int resolution, const int digits[]) {
    uint64_t cell = (1ULL << 63) | (1ULL << 59);
    cell |= (uint64_t(resolution) << 52);
    cell |= (uint64_t(base_cell & 0x7F) << 45);
    for (int r = 1; r <= 15; r++) {
        int shift = (15 - r) * 3 + 1;
        if (r <= resolution) {
            cell |= (uint64_t(digits[r - 1] & 0x7) << shift);
        } else {
            cell |= (7ULL << shift);
        }
    }
    return cell;
}

static void test_get_resolution() {
    // Build cells at various resolutions
    int d0[15] = {};
    uint64_t cells[5];
    cells[0] = make_h3_cell(1, 0, d0);   // res 0
    cells[1] = make_h3_cell(5, 3, d0);   // res 3
    cells[2] = make_h3_cell(10, 7, d0);  // res 7
    cells[3] = make_h3_cell(20, 11, d0); // res 11
    cells[4] = 0;                          // invalid

    int32_t results[5] = {};
    metal_status st = metal_h3_get_resolution(cells, 5, results);
    CHECK(st == METAL_OK, "get_resolution status=%d", st);
    CHECK(results[0] == 0, "res[0]=%d (expected 0)", results[0]);
    CHECK(results[1] == 3, "res[1]=%d (expected 3)", results[1]);
    CHECK(results[2] == 7, "res[2]=%d (expected 7)", results[2]);
    CHECK(results[3] == 11, "res[3]=%d (expected 11)", results[3]);
    CHECK(results[4] == -1, "res[4]=%d (expected -1, invalid)", results[4]);
}

static void test_cell_to_parent() {
    // Build a res-5 cell with digits [1,2,3,4,5]
    int digits[15] = {1, 2, 3, 4, 5};
    uint64_t cell = make_h3_cell(10, 5, digits);

    uint64_t cells[3] = { cell, cell, 0 };
    uint64_t parents[3] = {};

    // Truncate to res 3
    metal_status st = metal_h3_cell_to_parent(cells, 3, 3, parents);
    CHECK(st == METAL_OK, "cell_to_parent status=%d", st);

    // Parent should have res=3, digits [1,2,3], rest=7
    int32_t parent_res = (int32_t)((parents[0] >> 52) & 0xF);
    CHECK(parent_res == 3, "parent res=%d (expected 3)", parent_res);

    // Digit 4 (res index 4) should be 7 (unused)
    int shift4 = (15 - 4) * 3 + 1;
    int digit4 = (int)((parents[0] >> shift4) & 7);
    CHECK(digit4 == 7, "parent digit4=%d (expected 7, unused)", digit4);

    // Invalid cell should return 0
    CHECK(parents[2] == 0, "parent of invalid=%llu (expected 0)",
          (unsigned long long)parents[2]);
}

static void test_grid_distance_same() {
    int d1[15] = {1, 0, 0};
    int d2[15] = {1, 0, 0};
    uint64_t a = make_h3_cell(5, 3, d1);
    uint64_t b = make_h3_cell(5, 3, d2);

    uint64_t cells_a[1] = { a };
    uint64_t cells_b[1] = { b };
    int32_t dist[1] = { -99 };

    metal_status st = metal_h3_grid_distance(cells_a, cells_b, 1, dist);
    CHECK(st == METAL_OK, "grid_distance same status=%d", st);
    CHECK(dist[0] == 0, "dist same cell=%d (expected 0)", dist[0]);
}

static void test_grid_distance_different_base() {
    int d[15] = {0};
    uint64_t a = make_h3_cell(5, 3, d);
    uint64_t b = make_h3_cell(6, 3, d); // different base cell

    uint64_t cells_a[1] = { a };
    uint64_t cells_b[1] = { b };
    int32_t dist[1] = { 0 };

    metal_status st = metal_h3_grid_distance(cells_a, cells_b, 1, dist);
    CHECK(st == METAL_OK, "grid_distance diff_base status=%d", st);
    CHECK(dist[0] == -1, "dist diff_base=%d (expected -1)", dist[0]);
}

static void test_grid_distance_neighbors() {
    // Digit 0 = center, digit 1 = +i direction
    int d1[15] = {0, 0, 0};
    int d2[15] = {1, 0, 0};
    uint64_t a = make_h3_cell(5, 3, d1);
    uint64_t b = make_h3_cell(5, 3, d2);

    uint64_t cells_a[1] = { a };
    uint64_t cells_b[1] = { b };
    int32_t dist[1] = { -99 };

    metal_status st = metal_h3_grid_distance(cells_a, cells_b, 1, dist);
    CHECK(st == METAL_OK, "grid_distance neighbors status=%d", st);
    // Distance between center(0,0,0) and digit-1(1,0,0) at res 1
    // then two more zero digits → scaled distance
    CHECK(dist[0] > 0, "dist neighbors=%d (expected > 0)", dist[0]);
}

static void test_lat_lng_to_cell_basic() {
    // San Francisco: 37.7749, -122.4194
    double lats[3] = { 37.7749, 0.0, -33.8688 };
    double lngs[3] = { -122.4194, 0.0, 151.2093 };
    uint64_t cell_ids[3] = {};
    uint8_t valid[3] = {};

    metal_status st = metal_h3_lat_lng_to_cell(lats, lngs, 3, 5, cell_ids, valid);
    CHECK(st == METAL_OK, "lat_lng_to_cell status=%d", st);

    // All should be valid at res 5
    CHECK(valid[0] == 1, "SF valid=%d", valid[0]);
    CHECK(valid[1] == 1, "origin valid=%d", valid[1]);
    CHECK(valid[2] == 1, "Sydney valid=%d", valid[2]);

    // Cells should be non-zero
    CHECK(cell_ids[0] != 0, "SF cell=%llu (non-zero)", (unsigned long long)cell_ids[0]);
    CHECK(cell_ids[1] != 0, "origin cell=%llu (non-zero)", (unsigned long long)cell_ids[1]);
    CHECK(cell_ids[2] != 0, "Sydney cell=%llu (non-zero)", (unsigned long long)cell_ids[2]);

    // All cells should be different
    CHECK(cell_ids[0] != cell_ids[1] && cell_ids[1] != cell_ids[2],
          "all cells unique");

    // Resolution should be 5
    int res0 = (int)((cell_ids[0] >> 52) & 0xF);
    CHECK(res0 == 5, "SF cell res=%d (expected 5)", res0);
}

static void test_lat_lng_to_cell_res12_rejected() {
    // res >= 12 should be rejected (fp32 limit)
    double lats[1] = { 37.7749 };
    double lngs[1] = { -122.4194 };
    uint64_t cell_ids[1] = {};
    uint8_t valid[1] = {};

    metal_status st = metal_h3_lat_lng_to_cell(lats, lngs, 1, 12, cell_ids, valid);
    CHECK(st == METAL_ERROR, "lat_lng_to_cell res12 rejected=%d (expected error)", st);
}

static void test_lat_lng_bulk_10k() {
    const size_t N = 10000;
    std::vector<double> lats(N), lngs(N);
    std::vector<uint64_t> cell_ids(N);
    std::vector<uint8_t> valid(N);

    // Generate points in a grid
    for (size_t i = 0; i < N; i++) {
        lats[i] = -60.0 + 120.0 * (double)(i / 100) / 100.0;
        lngs[i] = -170.0 + 340.0 * (double)(i % 100) / 100.0;
    }

    metal_status st = metal_h3_lat_lng_to_cell(
        lats.data(), lngs.data(), N, 4, cell_ids.data(), valid.data());
    CHECK(st == METAL_OK, "lat_lng_to_cell 10K status=%d", st);

    size_t valid_count = 0;
    for (size_t i = 0; i < N; i++) {
        if (valid[i]) valid_count++;
    }
    // Most points should be valid (some edge cases near face boundaries may fail)
    CHECK(valid_count > N * 80 / 100,
          "lat_lng 10K: %zu/%zu valid (expected >80%%)", valid_count, N);
}

int main() {
    printf("=== Metal Backend H3 Tests ===\n\n");

    setenv("PGACCEL_METALLIB_PATH", "pgaccel_kernels.metallib", 1);

    metal_status st = metal_init();
    if (st != METAL_OK) {
        fprintf(stderr, "metal_init failed: %d\n", st);
        return 1;
    }

    metal_device_info info = metal_get_device_info();
    printf("Device: %s (unified=%d)\n\n", info.device_name, info.is_unified_memory);

    test_get_resolution();
    test_cell_to_parent();
    test_grid_distance_same();
    test_grid_distance_different_base();
    test_grid_distance_neighbors();
    test_lat_lng_to_cell_basic();
    test_lat_lng_to_cell_res12_rejected();
    test_lat_lng_bulk_10k();

    printf("\n=== Results: %d failures ===\n", failures);
    metal_shutdown();
    return failures > 0 ? 1 : 0;
}

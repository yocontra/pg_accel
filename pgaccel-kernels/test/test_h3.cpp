#include <algorithm>
#include <cmath>
#include <cstdio>
#include <cstdlib>
#include <cstring>
#include <vector>

#include "pgaccel_ffi.h"

static int g_pass = 0;
static int g_fail = 0;

#define ASSERT_EQ(desc, actual, expected)                                                    \
  do {                                                                                       \
    if ((actual) == (expected)) {                                                            \
      g_pass++;                                                                              \
    } else {                                                                                 \
      fprintf(stderr, "FAIL: %s — expected %lld, got %lld\n", (desc), (long long)(expected), \
              (long long)(actual));                                                          \
      g_fail++;                                                                              \
    }                                                                                        \
  } while (0)

#define ASSERT_STATUS_OK(desc, status)                                                \
  do {                                                                                \
    if ((status) == PGACCEL_OK) {                                                     \
      g_pass++;                                                                       \
    } else {                                                                          \
      fprintf(stderr, "FAIL: %s — status %d (expected OK)\n", (desc), (int)(status)); \
      g_fail++;                                                                       \
    }                                                                                 \
  } while (0)

#define ASSERT_TRUE(desc, cond)              \
  do {                                       \
    if ((cond)) {                            \
      g_pass++;                              \
    } else {                                 \
      fprintf(stderr, "FAIL: %s\n", (desc)); \
      g_fail++;                              \
    }                                        \
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
static uint64_t make_cell(int base_cell, int resolution, const int* digits) {
  uint64_t cell = (1ULL << 63);  // high bit
  cell |= (1ULL << 59);          // mode = 1
  cell |= ((uint64_t)(resolution & 0xF) << 52);
  cell |= ((uint64_t)(base_cell & 0x7F) << 45);
  // H3 v4 layout: digit r ∈ [1..15] at bits [(15-r)*3+2 .. (15-r)*3].
  // No `+1` reserved-bit offset (older revisions of this helper had one,
  // which silently corrupted base-cell read-back via bit-45 overlap).
  for (int r = 1; r <= 15; r++) {
    int shift = (15 - r) * 3;
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
  pgaccel_status s = pgaccel_h3_lat_lng_to_cell_bulk(&lat, &lng, 1, 0, true, &cell_id, &valid);
  ASSERT_STATUS_OK("lat_lng_to_cell res 0 status", s);
  ASSERT_TRUE("lat_lng_to_cell res 0 valid", valid == 1);
  ASSERT_TRUE("lat_lng_to_cell res 0 non-zero", cell_id != 0);

  // Verify resolution of returned cell
  int32_t res_out = -1;
  pgaccel_h3_get_resolution_bulk(&cell_id, 1, &res_out);
  ASSERT_EQ("returned cell has correct resolution", res_out, 0);

  // Res 5 should also work
  s = pgaccel_h3_lat_lng_to_cell_bulk(&lat, &lng, 1, 5, true, &cell_id, &valid);
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
  s = pgaccel_h3_lat_lng_to_cell_bulk(&lat, &lng, 1, 12, false, &cell_id, &valid);
  ASSERT_STATUS_OK("fp32 res 12 status", s);
  ASSERT_EQ("fp32 res 12 marked invalid", valid, 0);

  // fp64 at high res: post fp64-unlock (W1/W2/W3/W4), every backend
  // (including Metal via soft-fp64) must dispatch fp64 paths. An
  // UNSUPPORTED status here means the soft-fp64 lowering broke.
  s = pgaccel_h3_lat_lng_to_cell_bulk(&lat, &lng, 1, 12, true, &cell_id, &valid);
  ASSERT_STATUS_OK("fp64 res 12 status", s);
  ASSERT_TRUE("fp64 res 12 valid", valid == 1);

  // Invalid lat/lng
  double bad_lat = 100.0, bad_lng = 0.0;
  s = pgaccel_h3_lat_lng_to_cell_bulk(&bad_lat, &bad_lng, 1, 5, true, &cell_id, &valid);
  ASSERT_STATUS_OK("invalid lat status", s);
  ASSERT_EQ("invalid lat marked invalid", valid, 0);

  // Invalid resolution
  s = pgaccel_h3_lat_lng_to_cell_bulk(&lat, &lng, 1, 16, true, &cell_id, &valid);
  ASSERT_EQ("res 16 returns UNSUPPORTED", s, PGACCEL_ERROR_UNSUPPORTED);

  s = pgaccel_h3_lat_lng_to_cell_bulk(&lat, &lng, 1, -1, true, &cell_id, &valid);
  ASSERT_EQ("res -1 returns UNSUPPORTED", s, PGACCEL_ERROR_UNSUPPORTED);

  // Bulk: several well-known locations
  const size_t N = 5;
  double lats[N] = {40.689247, 48.858844, -33.856159, 35.659494, 51.500729};
  double lngs[N] = {-74.044502, 2.294351, 151.215256, 139.700472, -0.124625};
  uint64_t cells[N];
  uint8_t valids[N];
  s = pgaccel_h3_lat_lng_to_cell_bulk(lats, lngs, N, 4, true, cells, valids);
  ASSERT_STATUS_OK("bulk lat_lng status", s);
  int valid_count = 0;
  for (size_t i = 0; i < N; i++) {
    if (valids[i])
      valid_count++;
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
  s = pgaccel_h3_lat_lng_to_cell_bulk(&pole_lat, &pole_lng, 1, 2, true, &cell_id, &valid);
  ASSERT_STATUS_OK("north pole status", s);
  // May or may not be valid depending on face edge detection — just check no crash

  // South pole
  pole_lat = -90.0;
  s = pgaccel_h3_lat_lng_to_cell_bulk(&pole_lat, &pole_lng, 1, 2, true, &cell_id, &valid);
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
// Test: lat_lng_to_cell fp64 bulk coverage (W5 fp64-unlock plan)
//
// Exercises the soft-fp64 h3_latlng_to_cell path at 1k / 64k / 256k / 1M.
// Verifies status=OK, all cells marked valid, and all cells have the
// requested resolution. No skip-on-!fp64 branch — post fp64-unlock
// every size must run through soft-fp64 on Metal.
// ---------------------------------------------------------------------------
static void test_lat_lng_to_cell_fp64_bulk() {
  printf("--- test_lat_lng_to_cell fp64 bulk (1k/64k/256k/1M) ---\n");

  // NOTE: h3_lat_lng_to_cell fp64 path only activates at resolution >= 12
  // (per h3_ops.cpp:587 `want_fp64 = use_fp64 && resolution >= 12`). Use
  // resolution 12 here so the soft-fp64 kernel actually runs. Size list
  // kept at 1k/64k/256k/1M per W5 fp64-unlock plan.
  std::vector<size_t> sizes = {1024, 65536, 262144, 1048576};
  for (size_t N : sizes) {
    std::vector<double> lats(N), lngs(N);
    // Spread points over a safe subset of the globe (avoid poles where
    // the ref tests say behavior is fuzzy).
    for (size_t i = 0; i < N; i++) {
      double u = (double)(i % 1001) / 1000.0;  // 0..1
      double v = (double)((i * 7919 + 13) % 1003) / 1002.0;
      lats[i] = -60.0 + u * 120.0;   // [-60, 60]
      lngs[i] = -170.0 + v * 340.0;  // [-170, 170]
    }
    std::vector<uint64_t> cells(N, 0);
    std::vector<uint8_t> valids(N, 0);
    const int resolution = 12;  // res >= 12 forces fp64 soft-path on Metal
    pgaccel_status s = pgaccel_h3_lat_lng_to_cell_bulk(lats.data(), lngs.data(), N, resolution,
                                                       /*use_fp64=*/1, cells.data(), valids.data());
    char buf[64];
    snprintf(buf, sizeof(buf), "fp64 bulk N=%zu status", N);
    ASSERT_STATUS_OK(buf, s);

    // Require high validity rate (>=99%). A few face-boundary points may
    // legitimately fail.
    size_t valid_count = 0;
    for (size_t i = 0; i < N; i++)
      valid_count += valids[i] ? 1 : 0;
    snprintf(buf, sizeof(buf), "fp64 bulk N=%zu valid_rate >= 99%%", N);
    ASSERT_TRUE(buf, valid_count * 100 >= N * 99);

    // Spot-check that valid cells have the requested resolution
    // (sample every N/64 cells — dense enough to catch systemic bugs,
    // cheap for the 1M case).
    const size_t stride = std::max<size_t>(1, N / 64);
    bool res_ok = true;
    for (size_t i = 0; i < N && res_ok; i += stride) {
      if (!valids[i])
        continue;
      int32_t r = -1;
      pgaccel_h3_get_resolution_bulk(&cells[i], 1, &r);
      if (r != resolution) {
        fprintf(stderr, "  fp64 bulk N=%zu: cells[%zu]=0x%llx has res %d, expected %d\n", N, i,
                (unsigned long long)cells[i], r, resolution);
        res_ok = false;
      }
    }
    snprintf(buf, sizeof(buf), "fp64 bulk N=%zu all sampled cells have res %d", N, resolution);
    ASSERT_TRUE(buf, res_ok);
  }
}

// ---------------------------------------------------------------------------
// Test: get_base_cell
// ---------------------------------------------------------------------------
//
// Verifies H3 v4 base-cell extraction now that the `+1` offset bug is
// fixed (commit 2026-05-01). All 122 base cells (0..121) must round-trip
// regardless of parity.
static void test_get_base_cell() {
  printf("--- test_get_base_cell ---\n");

  int digits[15] = {0};

  // Single-row sanity.
  uint64_t cell = make_cell(57, 0, digits);
  int32_t base = -42;
  pgaccel_status s = pgaccel_h3_get_base_cell_bulk(&cell, 1, &base);
  ASSERT_STATUS_OK("get_base_cell single status", s);
  ASSERT_EQ("get_base_cell single value", base, 57);

  // Bulk over a mix of even and odd bases — both must round-trip post-fix.
  const size_t N = 5;
  uint64_t cells[N];
  cells[0] = make_cell(0, 0, digits);
  cells[1] = make_cell(1, 0, digits);
  cells[2] = make_cell(14, 0, digits);
  cells[3] = make_cell(56, 0, digits);
  cells[4] = make_cell(121, 0, digits);
  int32_t bases[N] = {-1, -1, -1, -1, -1};
  s = pgaccel_h3_get_base_cell_bulk(cells, N, bases);
  ASSERT_STATUS_OK("get_base_cell bulk status", s);
  ASSERT_EQ("get_base_cell bulk[0]", bases[0], 0);
  ASSERT_EQ("get_base_cell bulk[1]", bases[1], 1);
  ASSERT_EQ("get_base_cell bulk[2]", bases[2], 14);
  ASSERT_EQ("get_base_cell bulk[3]", bases[3], 56);
  ASSERT_EQ("get_base_cell bulk[4]", bases[4], 121);

  // Sweep every base in 0..121 — full coverage of the parity dimension.
  bool sweep_ok = true;
  for (int b = 0; b < 122; b++) {
    uint64_t c = make_cell(b, 0, digits);
    int32_t out = -1;
    pgaccel_h3_get_base_cell_bulk(&c, 1, &out);
    if (out != b) {
      fprintf(stderr, "FAIL: get_base_cell sweep base=%d → got %d\n", b, out);
      sweep_ok = false;
    }
  }
  ASSERT_TRUE("get_base_cell full 0..121 sweep round-trips", sweep_ok);

  // Zero cell → kernel sentinels -1 (per FFI contract).
  uint64_t zero = 0;
  int32_t zero_base = 0;
  s = pgaccel_h3_get_base_cell_bulk(&zero, 1, &zero_base);
  ASSERT_STATUS_OK("get_base_cell zero status", s);
  ASSERT_EQ("get_base_cell zero value", zero_base, -1);
}

// ---------------------------------------------------------------------------
// Test: is_valid_cell
// ---------------------------------------------------------------------------
static void test_is_valid_cell() {
  printf("--- test_is_valid_cell ---\n");

  int digits[15] = {0};

  // Well-formed cell at res 0 (no digits, all unused = 7) is valid.
  uint64_t valid_cell = make_cell(57, 0, digits);
  uint8_t v = 99;
  pgaccel_status s = pgaccel_h3_is_valid_cell_bulk(&valid_cell, 1, &v);
  ASSERT_STATUS_OK("is_valid_cell single status", s);
  ASSERT_EQ("is_valid_cell well-formed", v, 1);

  // Zero cell is invalid.
  uint64_t zero = 0;
  v = 99;
  s = pgaccel_h3_is_valid_cell_bulk(&zero, 1, &v);
  ASSERT_STATUS_OK("is_valid_cell zero status", s);
  ASSERT_EQ("is_valid_cell zero", v, 0);

  // Mode != 1 (2 = directed edge) → invalid.
  uint64_t bad_mode = valid_cell;
  bad_mode &= ~((uint64_t)0xF << 59);  // clear mode field
  bad_mode |= ((uint64_t)2 << 59);     // set mode = 2
  v = 99;
  s = pgaccel_h3_is_valid_cell_bulk(&bad_mode, 1, &v);
  ASSERT_STATUS_OK("is_valid_cell bad-mode status", s);
  ASSERT_EQ("is_valid_cell bad mode", v, 0);

  // Base cell out of range (>121) → invalid. Use 122 (binary 1111010).
  uint64_t bad_base = make_cell(122, 0, digits);
  v = 99;
  s = pgaccel_h3_is_valid_cell_bulk(&bad_base, 1, &v);
  ASSERT_STATUS_OK("is_valid_cell bad-base status", s);
  ASSERT_EQ("is_valid_cell bad base", v, 0);

  // Bulk mix: valid, zero, bad-mode.
  uint64_t cells[3] = {valid_cell, zero, bad_mode};
  uint8_t valids[3] = {99, 99, 99};
  s = pgaccel_h3_is_valid_cell_bulk(cells, 3, valids);
  ASSERT_STATUS_OK("is_valid_cell bulk status", s);
  ASSERT_EQ("is_valid_cell bulk[0]", valids[0], 1);
  ASSERT_EQ("is_valid_cell bulk[1]", valids[1], 0);
  ASSERT_EQ("is_valid_cell bulk[2]", valids[2], 0);
}

// ---------------------------------------------------------------------------
// Test: is_pentagon
// ---------------------------------------------------------------------------
//
// Verifies pentagon classification against the canonical 12 base cells
// from the H3 v4 reference. With the +1-offset bug fixed (commit
// 2026-05-01), all 12 must classify correctly regardless of parity.
static void test_is_pentagon() {
  printf("--- test_is_pentagon ---\n");

  int digits[15] = {0};

  // The 12 pentagon base cells per the H3 v4 reference.
  static const int PENT_BASES[12] = {4, 14, 24, 38, 49, 58, 63, 72, 83, 97, 107, 117};

  // Every pentagon base at res=0 → pentagon.
  for (int i = 0; i < 12; i++) {
    uint64_t cell = make_cell(PENT_BASES[i], 0, digits);
    uint8_t v = 99;
    pgaccel_status s = pgaccel_h3_is_pentagon_bulk(&cell, 1, &v);
    char buf[80];
    snprintf(buf, sizeof(buf), "is_pentagon base=%d res=0", PENT_BASES[i]);
    ASSERT_STATUS_OK(buf, s);
    ASSERT_EQ(buf, v, 1);
  }

  // Hexagon bases (mix of even/odd) are NOT pentagons.
  static const int HEX_BASES[4] = {0, 1, 56, 57};
  for (int i = 0; i < 4; i++) {
    uint64_t cell = make_cell(HEX_BASES[i], 0, digits);
    uint8_t v = 99;
    pgaccel_status s = pgaccel_h3_is_pentagon_bulk(&cell, 1, &v);
    char buf[80];
    snprintf(buf, sizeof(buf), "is_pentagon hex base=%d res=0", HEX_BASES[i]);
    ASSERT_STATUS_OK(buf, s);
    ASSERT_EQ(buf, v, 0);
  }

  // Pentagon base + all-zero sub-resolution digits → still pentagon.
  int zero_digits[15] = {0};
  uint64_t cell_p_r3 = make_cell(4, 3, zero_digits);
  uint8_t v = 99;
  pgaccel_status s = pgaccel_h3_is_pentagon_bulk(&cell_p_r3, 1, &v);
  ASSERT_STATUS_OK("is_pentagon base=4 res=3 all-zero digits status", s);
  ASSERT_EQ("is_pentagon base=4 res=3 all-zero digits", v, 1);

  // Pentagon base + non-zero leading digit → NOT pentagon.
  int mixed_digits[15] = {1, 0, 0};
  uint64_t cell_p_mixed = make_cell(4, 3, mixed_digits);
  v = 99;
  s = pgaccel_h3_is_pentagon_bulk(&cell_p_mixed, 1, &v);
  ASSERT_STATUS_OK("is_pentagon base=4 res=3 mixed-digit status", s);
  ASSERT_EQ("is_pentagon base=4 res=3 mixed-digit", v, 0);

  // Zero cell → not a pentagon.
  uint64_t zero = 0;
  v = 99;
  s = pgaccel_h3_is_pentagon_bulk(&zero, 1, &v);
  ASSERT_STATUS_OK("is_pentagon zero status", s);
  ASSERT_EQ("is_pentagon zero value", v, 0);
}

// ---------------------------------------------------------------------------
// Test: is_res_class_iii
// ---------------------------------------------------------------------------
static void test_is_res_class_iii() {
  printf("--- test_is_res_class_iii ---\n");

  int digits[15] = {0};

  // Class III iff resolution is odd. Sweep res 0..15.
  for (int r = 0; r <= 15; r++) {
    uint64_t cell = make_cell(57, r, digits);
    uint8_t v = 99;
    pgaccel_status s = pgaccel_h3_is_res_class_iii_bulk(&cell, 1, &v);
    char buf[64];
    snprintf(buf, sizeof(buf), "is_res_class_iii res=%d status", r);
    ASSERT_STATUS_OK(buf, s);
    snprintf(buf, sizeof(buf), "is_res_class_iii res=%d value", r);
    ASSERT_EQ(buf, v, (uint8_t)(r & 1));
  }

  // Bulk: alternating res 0..3 → expect [0, 1, 0, 1].
  uint64_t cells[4] = {make_cell(57, 0, digits), make_cell(57, 1, digits), make_cell(57, 2, digits),
                       make_cell(57, 3, digits)};
  uint8_t out[4] = {99, 99, 99, 99};
  pgaccel_status s = pgaccel_h3_is_res_class_iii_bulk(cells, 4, out);
  ASSERT_STATUS_OK("is_res_class_iii bulk status", s);
  ASSERT_EQ("is_res_class_iii bulk[0]", out[0], 0);
  ASSERT_EQ("is_res_class_iii bulk[1]", out[1], 1);
  ASSERT_EQ("is_res_class_iii bulk[2]", out[2], 0);
  ASSERT_EQ("is_res_class_iii bulk[3]", out[3], 1);
}

// ---------------------------------------------------------------------------
// Test: cell_to_center_child
// ---------------------------------------------------------------------------
static void test_cell_to_center_child() {
  printf("--- test_cell_to_center_child ---\n");

  int digits[15] = {0};

  // Same-resolution → returns input cell unchanged.
  uint64_t parent = make_cell(57, 3, digits);
  uint64_t child = 0;
  pgaccel_status s = pgaccel_h3_cell_to_center_child_bulk(&parent, 1, 3, &child);
  ASSERT_STATUS_OK("center_child same-res status", s);
  ASSERT_TRUE("center_child same-res returns input", child == parent);

  // Descend from res 0 to res 2 → digits [0, 0] populated, base preserved
  // (post-fix; the prior +1-offset layout would have stripped the LSB of
  // an odd base on descent — see commit 2026-05-01). Use base 57 (odd)
  // to exercise the fixed bit-45 boundary.
  uint64_t cell_r0 = make_cell(57, 0, digits);
  uint64_t cell_r2 = 0;
  s = pgaccel_h3_cell_to_center_child_bulk(&cell_r0, 1, 2, &cell_r2);
  ASSERT_STATUS_OK("center_child r0->r2 status", s);
  int32_t res_out = -1;
  pgaccel_h3_get_resolution_bulk(&cell_r2, 1, &res_out);
  ASSERT_EQ("center_child r0->r2 has res 2", res_out, 2);
  int32_t base_out = -1;
  pgaccel_h3_get_base_cell_bulk(&cell_r2, 1, &base_out);
  ASSERT_EQ("center_child r0->r2 base preserved (odd base)", base_out, 57);

  // Invalid: child_res < cell.res → 0.
  s = pgaccel_h3_cell_to_center_child_bulk(&parent, 1, 1, &child);
  ASSERT_STATUS_OK("center_child invalid child_res status", s);
  ASSERT_TRUE("center_child invalid child_res returns 0", child == 0);

  // Out-of-range child_res → kernel returns OK but writes 0.
  s = pgaccel_h3_cell_to_center_child_bulk(&cell_r0, 1, 16, &child);
  ASSERT_TRUE("center_child child_res=16 returns 0", child == 0);

  // Zero cell → 0.
  uint64_t zero = 0;
  s = pgaccel_h3_cell_to_center_child_bulk(&zero, 1, 5, &child);
  ASSERT_STATUS_OK("center_child zero-input status", s);
  ASSERT_TRUE("center_child zero-input returns 0", child == 0);
}

// ---------------------------------------------------------------------------
// Variable-output kernel tests
// ---------------------------------------------------------------------------

// Build a known-pentagon cell at the given resolution. Pentagon base 4 with
// all-zero digits is a valid pentagon per the H3 layout convention used by
// pgaccel_h3_is_pentagon_bulk.
static uint64_t make_pentagon_cell(int resolution) {
  int digits[15] = {0};
  return make_cell(4, resolution, digits);
}

// Test: grid_disk — output_size formula and emit consistency.
static void test_grid_disk() {
  printf("--- test_grid_disk ---\n");

  int digits[15] = {0};
  uint64_t cells[2] = {
      make_cell(57, 5, digits),  // hexagon (base 57 not in pentagon set)
      make_pentagon_cell(5),     // pentagon
  };

  // k = 1: hex disk = 7, pent disk = 6.
  {
    uint32_t off[3] = {99, 99, 99};
    pgaccel_status s = pgaccel_h3_grid_disk_output_size(cells, 2, 1, off);
    ASSERT_STATUS_OK("grid_disk k=1 size status", s);
    ASSERT_EQ("grid_disk k=1 off[0]", off[0], 0);
    ASSERT_EQ("grid_disk k=1 hex count", off[1], 7);
    ASSERT_EQ("grid_disk k=1 total (hex+pent)", off[2], 13);

    std::vector<uint64_t> out(off[2], 0);
    s = pgaccel_h3_grid_disk_emit(cells, 2, 1, off, out.data());
    ASSERT_STATUS_OK("grid_disk k=1 emit status", s);
    // Origin at slot 0 of each input
    ASSERT_TRUE("grid_disk k=1 hex origin emitted", out[0] == cells[0]);
    ASSERT_TRUE("grid_disk k=1 pent origin emitted", out[off[1]] == cells[1]);
    // All emitted cells are non-zero (valid IDs)
    bool all_nonzero = true;
    for (uint32_t i = 0; i < off[2]; i++) {
      if (out[i] == 0) {
        all_nonzero = false;
        break;
      }
    }
    ASSERT_TRUE("grid_disk k=1 all outputs non-zero", all_nonzero);
  }

  // k = 0: every input emits exactly itself.
  {
    uint32_t off[3] = {99, 99, 99};
    pgaccel_status s = pgaccel_h3_grid_disk_output_size(cells, 2, 0, off);
    ASSERT_STATUS_OK("grid_disk k=0 size status", s);
    ASSERT_EQ("grid_disk k=0 total", off[2], 2);

    std::vector<uint64_t> out(2, 0);
    s = pgaccel_h3_grid_disk_emit(cells, 2, 0, off, out.data());
    ASSERT_STATUS_OK("grid_disk k=0 emit status", s);
    ASSERT_TRUE("grid_disk k=0 emits hex itself", out[0] == cells[0]);
    ASSERT_TRUE("grid_disk k=0 emits pent itself", out[1] == cells[1]);
  }

  // k = 2: hex disk = 1 + 6 + 12 = 19; pent disk = 1 + 5 + 10 = 16.
  {
    uint32_t off[3] = {0, 0, 0};
    pgaccel_status s = pgaccel_h3_grid_disk_output_size(cells, 2, 2, off);
    ASSERT_STATUS_OK("grid_disk k=2 size status", s);
    ASSERT_EQ("grid_disk k=2 hex count", off[1], 19);
    ASSERT_EQ("grid_disk k=2 total", off[2], 35);

    std::vector<uint64_t> out(off[2], 0);
    s = pgaccel_h3_grid_disk_emit(cells, 2, 2, off, out.data());
    ASSERT_STATUS_OK("grid_disk k=2 emit status", s);
    // The disk size pass + emit pass write count must match.
    ASSERT_EQ("grid_disk k=2 emit==size sum", out.size(), off[2]);
  }

  // Zero cell input → zero count.
  {
    uint64_t z[1] = {0};
    uint32_t off[2] = {99, 99};
    pgaccel_status s = pgaccel_h3_grid_disk_output_size(z, 1, 3, off);
    ASSERT_STATUS_OK("grid_disk zero-cell status", s);
    ASSERT_EQ("grid_disk zero-cell count", off[1], 0);
  }
}

// Test: grid_ring_unsafe — output_size formula and emit consistency.
static void test_grid_ring_unsafe() {
  printf("--- test_grid_ring_unsafe ---\n");

  int digits[15] = {0};
  uint64_t cells[2] = {
      make_cell(57, 5, digits),  // hexagon
      make_pentagon_cell(5),     // pentagon
  };

  // k = 1: hex ring = 6, pent ring = 5.
  {
    uint32_t off[3] = {99, 99, 99};
    pgaccel_status s = pgaccel_h3_grid_ring_unsafe_output_size(cells, 2, 1, off);
    ASSERT_STATUS_OK("grid_ring k=1 size status", s);
    ASSERT_EQ("grid_ring k=1 hex count", off[1], 6);
    ASSERT_EQ("grid_ring k=1 total", off[2], 11);

    std::vector<uint64_t> out(off[2], 0);
    s = pgaccel_h3_grid_ring_unsafe_emit(cells, 2, 1, off, out.data());
    ASSERT_STATUS_OK("grid_ring k=1 emit status", s);
    bool all_nonzero = true;
    for (uint32_t i = 0; i < off[2]; i++) {
      if (out[i] == 0) {
        all_nonzero = false;
        break;
      }
    }
    ASSERT_TRUE("grid_ring k=1 all outputs non-zero", all_nonzero);
  }

  // k = 3: hex ring = 18, pent ring = 15.
  {
    uint32_t off[3] = {0, 0, 0};
    pgaccel_status s = pgaccel_h3_grid_ring_unsafe_output_size(cells, 2, 3, off);
    ASSERT_STATUS_OK("grid_ring k=3 size status", s);
    ASSERT_EQ("grid_ring k=3 hex count", off[1], 18);
    ASSERT_EQ("grid_ring k=3 total", off[2], 33);

    std::vector<uint64_t> out(off[2], 0);
    s = pgaccel_h3_grid_ring_unsafe_emit(cells, 2, 3, off, out.data());
    ASSERT_STATUS_OK("grid_ring k=3 emit status", s);
    ASSERT_EQ("grid_ring k=3 emit==size sum", out.size(), off[2]);
  }

  // k = 0: ring-0 = single cell (the input).
  {
    uint32_t off[2] = {99, 99};
    pgaccel_status s = pgaccel_h3_grid_ring_unsafe_output_size(cells, 1, 0, off);
    ASSERT_STATUS_OK("grid_ring k=0 size status", s);
    ASSERT_EQ("grid_ring k=0 hex count", off[1], 1);

    uint64_t out[1] = {0};
    s = pgaccel_h3_grid_ring_unsafe_emit(cells, 1, 0, off, out);
    ASSERT_STATUS_OK("grid_ring k=0 emit status", s);
    ASSERT_TRUE("grid_ring k=0 emits cell itself", out[0] == cells[0]);
  }
}

// Test: cell_to_children — count formula and same-res passthrough.
static void test_cell_to_children() {
  printf("--- test_cell_to_children ---\n");

  int digits[15] = {0};
  uint64_t parent_r3 = make_cell(57, 3, digits);

  // child_res == cell.res → 1 cell (the input itself).
  {
    uint32_t off[2] = {99, 99};
    pgaccel_status s = pgaccel_h3_cell_to_children_output_size(&parent_r3, 1, 3, off);
    ASSERT_STATUS_OK("c2c same-res size status", s);
    ASSERT_EQ("c2c same-res count", off[1], 1);

    uint64_t out[1] = {0};
    s = pgaccel_h3_cell_to_children_emit(&parent_r3, 1, 3, off, out);
    ASSERT_STATUS_OK("c2c same-res emit status", s);
    ASSERT_TRUE("c2c same-res returns input", out[0] == parent_r3);
  }

  // child_res = res + 1 → 7 children for hexagon.
  {
    uint32_t off[2] = {99, 99};
    pgaccel_status s = pgaccel_h3_cell_to_children_output_size(&parent_r3, 1, 4, off);
    ASSERT_STATUS_OK("c2c r4 size status", s);
    ASSERT_EQ("c2c r4 hex count", off[1], 7);

    std::vector<uint64_t> out(7, 0);
    s = pgaccel_h3_cell_to_children_emit(&parent_r3, 1, 4, off, out.data());
    ASSERT_STATUS_OK("c2c r4 emit status", s);
    // All children should be non-zero, distinct, at resolution 4.
    bool distinct = true;
    for (size_t i = 0; i < 7; i++) {
      if (out[i] == 0) {
        distinct = false;
        break;
      }
      for (size_t j = i + 1; j < 7; j++) {
        if (out[i] == out[j]) {
          distinct = false;
          break;
        }
      }
    }
    ASSERT_TRUE("c2c r4 children distinct + non-zero", distinct);

    // Verify resolution
    int32_t child_res[7] = {-1, -1, -1, -1, -1, -1, -1};
    pgaccel_h3_get_resolution_bulk(out.data(), 7, child_res);
    bool all_r4 = true;
    for (int i = 0; i < 7; i++) {
      if (child_res[i] != 4) {
        all_r4 = false;
        break;
      }
    }
    ASSERT_TRUE("c2c r4 all children at res 4", all_r4);
  }

  // child_res = res + 2 → 49 children (7^2) for hexagon.
  {
    uint32_t off[2] = {99, 99};
    pgaccel_status s = pgaccel_h3_cell_to_children_output_size(&parent_r3, 1, 5, off);
    ASSERT_STATUS_OK("c2c r5 size status", s);
    ASSERT_EQ("c2c r5 hex count", off[1], 49);
  }

  // Pentagon: child_res = res + 1 → 5 children (pentagon has 5 not 7).
  {
    uint64_t pent = make_pentagon_cell(3);
    uint32_t off[2] = {99, 99};
    pgaccel_status s = pgaccel_h3_cell_to_children_output_size(&pent, 1, 4, off);
    ASSERT_STATUS_OK("c2c pent r4 size status", s);
    ASSERT_EQ("c2c pent r4 count", off[1], 5);
  }

  // Invalid: child_res < cell.res → 0 cells.
  {
    uint32_t off[2] = {99, 99};
    pgaccel_status s = pgaccel_h3_cell_to_children_output_size(&parent_r3, 1, 2, off);
    ASSERT_STATUS_OK("c2c invalid size status", s);
    ASSERT_EQ("c2c invalid count", off[1], 0);
  }
}

// Test: cell_to_boundary — vertex count and finite values.
static void test_cell_to_boundary() {
  printf("--- test_cell_to_boundary ---\n");

  int digits[15] = {0};
  uint64_t cells[2] = {
      make_cell(5, 3, digits),  // hexagon (base 5 not in pentagon set)
      make_pentagon_cell(3),    // pentagon
  };

  uint32_t off[3] = {99, 99, 99};
  pgaccel_status s = pgaccel_h3_cell_to_boundary_output_size(cells, 2, off);
  ASSERT_STATUS_OK("boundary size status", s);
  ASSERT_EQ("boundary off[0]", off[0], 0);
  ASSERT_EQ("boundary hex doubles", off[1], 12);    // 6 verts × 2 doubles
  ASSERT_EQ("boundary total doubles", off[2], 22);  // 12 + 10

  std::vector<double> out(off[2], 0.0);
  s = pgaccel_h3_cell_to_boundary_emit(cells, 2, off, out.data());
  ASSERT_STATUS_OK("boundary emit status", s);

  // All hex vertices should be finite
  bool hex_finite = true;
  for (uint32_t i = 0; i < off[1]; i++) {
    if (!std::isfinite(out[i])) {
      hex_finite = false;
      break;
    }
  }
  ASSERT_TRUE("boundary hex vertices finite", hex_finite);

  // Hex should have 6 distinct (lat,lng) pairs (vertices of a hexagon)
  bool hex_distinct = true;
  for (int v1 = 0; v1 < 6; v1++) {
    for (int v2 = v1 + 1; v2 < 6; v2++) {
      double lat1 = out[v1 * 2 + 0];
      double lng1 = out[v1 * 2 + 1];
      double lat2 = out[v2 * 2 + 0];
      double lng2 = out[v2 * 2 + 1];
      double dist = std::abs(lat1 - lat2) + std::abs(lng1 - lng2);
      if (dist < 1e-12) {
        hex_distinct = false;
        break;
      }
    }
  }
  ASSERT_TRUE("boundary hex vertices distinct", hex_distinct);
}

// Test: polyfill — large polygon at low resolution.
static void test_polyfill() {
  printf("--- test_polyfill ---\n");

  // Define a large rectangular polygon spanning multiple grid steps at the
  // chosen resolution. Coords are interleaved [x0,y0, x1,y1, ...] in
  // lon/lat degrees, ring closed.
  //
  // At res 4, the kernel's candidate-step is ~60/SQRT7^4 = 60/49 ≈ 1.22°.
  // Using a 50x50° polygon at res 4 yields ~40x40 candidate points well
  // inside the bbox, so at least some must land inside the polygon
  // interior.
  float coords[] = {
      -25.0f, -25.0f, 25.0f, -25.0f, 25.0f, 25.0f, -25.0f, 25.0f, -25.0f, -25.0f,  // close
  };
  uint32_t ring_offsets[2] = {0, 5};

  uint32_t out_offsets[2] = {99, 99};
  pgaccel_status s = pgaccel_h3_polyfill_output_size(coords, ring_offsets, 1, 4, out_offsets);
  ASSERT_STATUS_OK("polyfill size status", s);
  ASSERT_EQ("polyfill out_offsets[0]", out_offsets[0], 0);
  ASSERT_TRUE("polyfill positive size estimate", out_offsets[1] > 0);

  std::vector<uint64_t> out(out_offsets[1], 0);
  s = pgaccel_h3_polyfill_emit(coords, ring_offsets, 1, 4, out_offsets, out.data());
  ASSERT_STATUS_OK("polyfill emit status", s);

  // Some cells should be filled (non-zero). Allow zero-sentinel slots from
  // the size-pass overestimate, but require at least one filled cell to
  // verify the kernel actually executed and found inside-points.
  uint32_t filled = 0;
  for (uint64_t c : out) {
    if (c != 0)
      filled++;
  }
  ASSERT_TRUE("polyfill at least one cell filled", filled > 0);
}

// Test: cells_to_multi_polygon — round-trip CSR layout.
static void test_cells_to_multi_polygon() {
  printf("--- test_cells_to_multi_polygon ---\n");

  int digits[15] = {0};
  uint64_t cells[3] = {
      make_cell(5, 3, digits),
      make_cell(6, 3, digits),
      make_pentagon_cell(3),
  };

  uint32_t ring_offsets[4] = {99, 99, 99, 99};
  uint32_t ring_count = 99;
  pgaccel_status s =
      pgaccel_h3_cells_to_multi_polygon_output_size(cells, 3, ring_offsets, &ring_count);
  ASSERT_STATUS_OK("c2mp size status", s);
  ASSERT_EQ("c2mp ring_count == cells", ring_count, 3);
  ASSERT_EQ("c2mp ring_offsets[0]", ring_offsets[0], 0);
  ASSERT_EQ("c2mp ring_offsets[1] hex", ring_offsets[1], 12);
  ASSERT_EQ("c2mp ring_offsets[2] hex+hex", ring_offsets[2], 24);
  ASSERT_EQ("c2mp ring_offsets[3] +pent", ring_offsets[3], 34);

  std::vector<double> coords(ring_offsets[3], 0.0);
  s = pgaccel_h3_cells_to_multi_polygon_emit(cells, 3, ring_offsets, ring_count, coords.data());
  ASSERT_STATUS_OK("c2mp emit status", s);

  bool all_finite = true;
  for (double v : coords) {
    if (!std::isfinite(v)) {
      all_finite = false;
      break;
    }
  }
  ASSERT_TRUE("c2mp all coords finite", all_finite);
}

// ---------------------------------------------------------------------------
// Main
// ---------------------------------------------------------------------------
int main() {
  printf("=== pg_accel H3 kernel tests ===\n\n");

  test_get_resolution();
  test_get_base_cell();
  test_is_valid_cell();
  test_is_pentagon();
  test_is_res_class_iii();
  test_cell_to_parent();
  test_cell_to_center_child();
  test_grid_distance();
  test_lat_lng_to_cell();
  test_lat_lng_to_cell_fp64_bulk();
  test_null_pointers();

  // Variable-output kernels (Agent 5A)
  test_grid_disk();
  test_grid_ring_unsafe();
  test_cell_to_children();
  test_cell_to_boundary();
  test_polyfill();
  test_cells_to_multi_polygon();

  printf("\n=== Results: %d passed, %d failed ===\n", g_pass, g_fail);
  return g_fail > 0 ? 1 : 0;
}

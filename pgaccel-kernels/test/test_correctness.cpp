#include <algorithm>
#include <cmath>
#include <cstdio>
#include <cstdlib>
#include <cstring>
#include <limits>
#include <random>
#include <vector>

#include "pgaccel_ffi.h"
#include "pgaccel_hash_join.h"

// ---------------------------------------------------------------------------
// Minimal test harness
// ---------------------------------------------------------------------------

static int g_tests_run = 0;
static int g_tests_passed = 0;
static int g_tests_failed = 0;

#define ASSERT_EQ(desc, got, expected)                                                    \
  do {                                                                                    \
    g_tests_run++;                                                                        \
    if ((got) == (expected)) {                                                            \
      g_tests_passed++;                                                                   \
    } else {                                                                              \
      g_tests_failed++;                                                                   \
      fprintf(stderr, "  FAIL: %s — got %lld, expected %lld\n", (desc), (long long)(got), \
              (long long)(expected));                                                     \
    }                                                                                     \
  } while (0)

#define ASSERT_NEAR(desc, got, expected, tol)                                          \
  do {                                                                                 \
    g_tests_run++;                                                                     \
    if (std::abs((double)(got) - (double)(expected)) <= (double)(tol)) {               \
      g_tests_passed++;                                                                \
    } else {                                                                           \
      g_tests_failed++;                                                                \
      fprintf(stderr, "  FAIL: %s — got %.6f, expected %.6f\n", (desc), (double)(got), \
              (double)(expected));                                                     \
    }                                                                                  \
  } while (0)

#define ASSERT_TRUE(desc, cond)                \
  do {                                         \
    g_tests_run++;                             \
    if ((cond)) {                              \
      g_tests_passed++;                        \
    } else {                                   \
      g_tests_failed++;                        \
      fprintf(stderr, "  FAIL: %s\n", (desc)); \
    }                                          \
  } while (0)

#define ASSERT_STATUS_OK(desc, status) ASSERT_EQ(desc, status, PGACCEL_OK)

// ---------------------------------------------------------------------------
// H3 helper: build cell ID from base cell + resolution + digits
// ---------------------------------------------------------------------------

static uint64_t make_cell(int base_cell, int resolution, const int* digits) {
  uint64_t cell = 0;
  cell |= (1ULL << 59);
  cell |= ((uint64_t)(resolution & 0xF) << 52);
  cell |= ((uint64_t)(base_cell & 0x7F) << 45);
  // H3 v4 layout: digit r in [1..15] occupies bits
  // [(15-r)*3+2 .. (15-r)*3]. There is no reserved-bit offset here.
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
// Raster helper: build expression instructions
// ---------------------------------------------------------------------------

static pgaccel_expr_inst make_load_band(int idx) {
  pgaccel_expr_inst inst;
  inst.op = PGACCEL_OP_LOAD_BAND;
  inst.arg.band_index = idx;
  return inst;
}

static pgaccel_expr_inst make_load_const(double val) {
  pgaccel_expr_inst inst;
  inst.op = PGACCEL_OP_LOAD_CONST;
  inst.arg.constant = val;
  return inst;
}

static pgaccel_expr_inst make_op(pgaccel_op op) {
  pgaccel_expr_inst inst;
  inst.op = op;
  inst.arg.constant = 0.0;
  return inst;
}

// ---------------------------------------------------------------------------
// Dispatch helper: check pair presence
// ---------------------------------------------------------------------------

static bool has_pair(const uint32_t* pairs, size_t count, uint32_t i, uint32_t j) {
  for (size_t k = 0; k < count; ++k) {
    if (pairs[k * 2] == i && pairs[k * 2 + 1] == j)
      return true;
  }
  return false;
}

struct DispatchResult {
  std::vector<uint32_t> dt, df, unc;
  size_t dt_count = 0, df_count = 0, unc_count = 0;
  explicit DispatchResult(size_t max_pairs)
      : dt(max_pairs * 2), df(max_pairs * 2), unc(max_pairs * 2) {}
};

/* Test-only compatibility adapter for the legacy pair-bucket assertions in
 * this file. Production callers use the linear pairwise ABI directly. */
static pgaccel_status
test_spatial_cross_product(const pgaccel_geometry* geoms_a, size_t count_a,
                           const pgaccel_geometry* geoms_b, size_t count_b,
                           uint32_t* definite_true_pairs, size_t* definite_true_count,
                           uint32_t* definite_false_pairs, size_t* definite_false_count,
                           uint32_t* uncertain_pairs, size_t* uncertain_count) {
  *definite_true_count = 0;
  *definite_false_count = 0;
  *uncertain_count = 0;
  if (count_a == 0 || count_b == 0)
    return PGACCEL_OK;
  if (count_a > std::numeric_limits<size_t>::max() / count_b)
    return PGACCEL_ERROR;

  const size_t count = count_a * count_b;
  std::vector<pgaccel_geometry> pair_a;
  std::vector<pgaccel_geometry> pair_b;
  pair_a.reserve(count);
  pair_b.reserve(count);
  for (size_t i = 0; i < count_a; ++i) {
    for (size_t j = 0; j < count_b; ++j) {
      pair_a.push_back(geoms_a[i]);
      pair_b.push_back(geoms_b[j]);
    }
  }

  std::vector<int8_t> results(count, 99);
  const pgaccel_status status =
      pgaccel_spatial_intersects_pairwise(pair_a.data(), pair_b.data(), count, results.data());
  if (status != PGACCEL_OK)
    return status;

  for (size_t k = 0; k < count; ++k) {
    const uint32_t i = static_cast<uint32_t>(k / count_b);
    const uint32_t j = static_cast<uint32_t>(k % count_b);
    uint32_t* pairs = nullptr;
    size_t* pair_count = nullptr;
    if (results[k] == 1) {
      pairs = definite_true_pairs;
      pair_count = definite_true_count;
    } else if (results[k] == -1) {
      pairs = definite_false_pairs;
      pair_count = definite_false_count;
    } else {
      pairs = uncertain_pairs;
      pair_count = uncertain_count;
    }
    pairs[*pair_count * 2] = i;
    pairs[*pair_count * 2 + 1] = j;
    ++*pair_count;
  }
  return PGACCEL_OK;
}

// =========================================================================
// HASH JOIN EDGE CASES
// =========================================================================

static void test_hash_join_large_threshold_correctness() {
  printf("--- hash_join: 100k inner rows uses only safe GPU/debug path ---\n");
  constexpr size_t N = 100000;
  constexpr size_t DUP_KEYS = 16;
  constexpr size_t PER_KEY = N / DUP_KEYS;
  std::vector<int32_t> inner(N);
  std::vector<uint32_t> indices(N);
  for (size_t i = 0; i < N; ++i) {
    inner[i] = static_cast<int32_t>(i % DUP_KEYS);
    indices[i] = static_cast<uint32_t>(i);
  }

  pgaccel_hash_table* ht =
      pgaccel_hash_join_build(inner.data(), nullptr, indices.data(), N, PGACCEL_KEY_INT32);
  if (ht == nullptr) {
    ASSERT_TRUE("large hash join build declined without safe GPU path", true);
    return;
  }

  int32_t outer[] = {7, 15, 99};
  const size_t expected = PER_KEY * 2;
  std::vector<uint32_t> pairs(expected * 2, UINT32_MAX);
  size_t match_count = 0;
  pgaccel_status st =
      pgaccel_hash_join_probe(ht, outer, nullptr, 3, pairs.data(), expected, &match_count);
  ASSERT_STATUS_OK("large hash join probe", st);
  ASSERT_EQ("large hash join match count", match_count, expected);

  bool valid_pairs = true;
  size_t outer0 = 0;
  size_t outer1 = 0;
  size_t outer2 = 0;
  for (size_t i = 0; i < match_count && i < expected; ++i) {
    const uint32_t oi = pairs[i * 2];
    const uint32_t ii = pairs[i * 2 + 1];
    if (ii >= inner.size() || oi >= 3 || inner[ii] != outer[oi]) {
      valid_pairs = false;
      break;
    }
    if (oi == 0)
      outer0++;
    else if (oi == 1)
      outer1++;
    else if (oi == 2)
      outer2++;
  }
  ASSERT_TRUE("large hash join output pairs valid", valid_pairs);
  ASSERT_EQ("large hash join key 7 matches", outer0, PER_KEY);
  ASSERT_EQ("large hash join key 15 matches", outer1, PER_KEY);
  ASSERT_EQ("large hash join missing key matches", outer2, 0);
  pgaccel_hash_join_free(ht);
}

static void test_hash_join_small_build_is_supported() {
  printf("--- hash_join: small build is supported ---\n");
  int32_t inner[] = {5, 5, 7};
  uint32_t indices[] = {0, 1, 2};
  pgaccel_hash_table* ht = pgaccel_hash_join_build(inner, nullptr, indices, 3, PGACCEL_KEY_INT32);
  ASSERT_TRUE("small hash join build returns a table", ht != nullptr);
  if (ht != nullptr)
    pgaccel_hash_join_free(ht);
}

static void test_hash_join_max_matches_overflow_rejected() {
  printf("--- hash_join: impossible max_matches rejected before writes ---\n");
  int32_t inner[] = {1};
  uint32_t indices[] = {0};
  pgaccel_hash_table* ht = pgaccel_hash_join_build(inner, nullptr, indices, 1, PGACCEL_KEY_INT32);
  if (ht == nullptr) {
    ASSERT_TRUE("overflow-capacity build declined without safe GPU path", true);
    return;
  }

  int32_t outer[] = {1};
  uint32_t pairs[] = {123, 456};
  size_t match_count = 999;
  pgaccel_status st = pgaccel_hash_join_probe(ht, outer, nullptr, 1, pairs,
                                              std::numeric_limits<size_t>::max(), &match_count);
  ASSERT_EQ("overflow-capacity probe status", st, PGACCEL_UNSUPPORTED);
  ASSERT_EQ("overflow-capacity match_count reset", match_count, 0);
  ASSERT_EQ("overflow-capacity pair[0] unchanged", pairs[0], 123);
  ASSERT_EQ("overflow-capacity pair[1] unchanged", pairs[1], 456);
  pgaccel_hash_join_free(ht);
}

// =========================================================================
// SPATIAL PREDICATE EDGE CASES
// =========================================================================

// Unit square ring
static const double square_ring[] = {
    0.0, 0.0, 1.0, 0.0, 1.0, 1.0, 0.0, 1.0, 0.0, 0.0,
};
static const size_t square_verts = 5;

static void test_point_in_ring_on_edge() {
  printf("--- point_in_ring: point exactly on edge ---\n");
  double pt[] = {0.5, 0.0};
  int8_t result = 99;
  pgaccel_point_in_ring_bulk(pt, 1, square_ring, square_verts, true, &result);
  ASSERT_EQ("on bottom edge -> uncertain", result, 0);
}

static void test_point_in_ring_on_vertex() {
  printf("--- point_in_ring: point exactly on vertex ---\n");
  double vertices[][2] = {{0.0, 0.0}, {1.0, 0.0}, {1.0, 1.0}, {0.0, 1.0}};
  for (int i = 0; i < 4; i++) {
    int8_t result = 99;
    pgaccel_point_in_ring_bulk(vertices[i], 1, square_ring, square_verts, true, &result);
    char desc[64];
    snprintf(desc, sizeof(desc), "vertex %d -> uncertain", i);
    ASSERT_EQ(desc, result, 0);
  }
}

static void test_point_in_ring_degenerate_2pts() {
  printf("--- point_in_ring: degenerate ring (2 points) ---\n");
  double degen[] = {0.0, 0.0, 1.0, 1.0};
  double pt[] = {0.5, 0.5};
  int8_t result = 99;
  pgaccel_point_in_ring_bulk(pt, 1, degen, 2, true, &result);
  ASSERT_EQ("2-point ring -> uncertain", result, 0);
}

static void test_point_in_ring_identical_vertices() {
  printf("--- point_in_ring: all identical vertices ---\n");
  double degen[] = {1.0, 1.0, 1.0, 1.0, 1.0, 1.0, 1.0, 1.0, 1.0, 1.0};
  double pt[] = {1.0, 1.0};
  int8_t result = 99;
  pgaccel_point_in_ring_bulk(pt, 1, degen, 5, true, &result);
  ASSERT_EQ("identical vertices -> uncertain", result, 0);

  double pt2[] = {2.0, 2.0};
  int8_t result2 = 99;
  pgaccel_point_in_ring_bulk(pt2, 1, degen, 5, true, &result2);
  // Could be uncertain or outside; definitely not inside
  ASSERT_TRUE("identical vertices, outside pt != inside", result2 != 1);
}

static void test_point_in_ring_large_circle() {
  printf("--- point_in_ring: large ring (1000 vertices circle) ---\n");
  const size_t N = 1001;  // closed ring
  std::vector<double> ring(N * 2);
  double cx = 50.0, cy = 50.0, radius = 40.0;
  for (size_t i = 0; i < N; i++) {
    double angle = 2.0 * M_PI * (double)i / (double)(N - 1);
    ring[i * 2] = cx + radius * std::cos(angle);
    ring[i * 2 + 1] = cy + radius * std::sin(angle);
  }

  // Center: inside
  {
    double pt[] = {50.0, 50.0};
    int8_t result = 0;
    pgaccel_status s = pgaccel_point_in_ring_bulk(pt, 1, ring.data(), N, true, &result);
    ASSERT_STATUS_OK("large circle status", s);
    ASSERT_EQ("center of circle -> inside", result, 1);
  }
  // Well outside
  {
    double pt[] = {200.0, 200.0};
    int8_t result = 0;
    pgaccel_point_in_ring_bulk(pt, 1, ring.data(), N, true, &result);
    ASSERT_EQ("far outside circle -> outside", result, -1);
  }
  // Just inside
  {
    double pt[] = {50.0 + 39.0, 50.0};
    int8_t result = 0;
    pgaccel_point_in_ring_bulk(pt, 1, ring.data(), N, true, &result);
    ASSERT_EQ("just inside circle -> inside", result, 1);
  }
  // Just outside
  {
    double pt[] = {50.0 + 41.0, 50.0};
    int8_t result = 0;
    pgaccel_point_in_ring_bulk(pt, 1, ring.data(), N, true, &result);
    ASSERT_EQ("just outside circle -> outside", result, -1);
  }
}

static void test_point_in_ring_bulk_0_points() {
  printf("--- point_in_ring: bulk with 0 points ---\n");
  pgaccel_status s =
      pgaccel_point_in_ring_bulk(nullptr, 0, square_ring, square_verts, true, nullptr);
  ASSERT_STATUS_OK("0 points OK", s);
}

static void test_point_in_ring_bulk_1_point() {
  printf("--- point_in_ring: bulk with 1 point ---\n");
  double pt[] = {0.5, 0.5};
  int8_t result = 0;
  pgaccel_status s = pgaccel_point_in_ring_bulk(pt, 1, square_ring, square_verts, true, &result);
  ASSERT_STATUS_OK("1 point status", s);
  ASSERT_EQ("1 point inside", result, 1);
}

// ---------------------------------------------------------------------------
// sphere_distance edge cases
// ---------------------------------------------------------------------------

// fp32 path remains covered here for edge-case behavior. The fp64
// sphere-distance kernel is covered separately by the fp64 unlock and
// fork-cold suites; it no longer returns PGACCEL_ERROR_NO_DEVICE solely
// because the device is Metal.
static void test_sphere_distance_identical() {
  printf("--- sphere_distance: identical points (distance=0) ---\n");
  float a[] = {10.0f, 20.0f};
  float b[] = {10.0f, 20.0f};
  float dist = -1;
  uint8_t unc = 0;
  pgaccel_sphere_distance_bulk(a, b, 1, false, &dist, &unc);
  // Identical points produce uncertain (near-zero threshold)
  ASSERT_EQ("identical points -> uncertain", unc, 1);
}

static void test_sphere_distance_antipodal() {
  printf("--- sphere_distance: antipodal points ---\n");
  float a[] = {0.0f, 0.0f};
  float b[] = {180.0f, 0.0f};
  float dist = 0;
  uint8_t unc = 0;
  pgaccel_sphere_distance_bulk(a, b, 1, false, &dist, &unc);
  ASSERT_EQ("antipodal -> uncertain", unc, 1);
}

static void test_sphere_distance_near_antipodal() {
  printf("--- sphere_distance: near-antipodal points ---\n");
  float a[] = {0.0f, 0.0f};
  float b[] = {179.99f, 0.0f};
  float dist = 0;
  uint8_t unc = 0;
  pgaccel_sphere_distance_bulk(a, b, 1, false, &dist, &unc);
  // Near-antipodal: could be uncertain due to numerical instability
  // or definite with a large distance. Either is acceptable.
  ASSERT_TRUE("near-antipodal no crash", true);
  if (unc == 0) {
    // If definite, distance should be close to half circumference (~20015 km)
    ASSERT_NEAR("near-antipodal ~20000km", dist / 1000.0f, 20015.0f, 100.0f);
  }
}

static void test_sphere_distance_poles() {
  printf("--- sphere_distance: points at poles ---\n");
  // North pole to south pole
  {
    float a[] = {0.0f, 90.0f};
    float b[] = {0.0f, -90.0f};
    float dist = 0;
    uint8_t unc = 0;
    pgaccel_sphere_distance_bulk(a, b, 1, false, &dist, &unc);
    // Pole to pole is antipodal
    ASSERT_EQ("pole-to-pole -> uncertain", unc, 1);
  }
  // North pole to equator
  {
    float a[] = {0.0f, 90.0f};
    float b[] = {0.0f, 0.0f};
    float dist = 0;
    uint8_t unc = 0;
    pgaccel_sphere_distance_bulk(a, b, 1, false, &dist, &unc);
    ASSERT_EQ("pole-to-equator definite", unc, 0);
    // Quarter circumference ~10008 km
    ASSERT_NEAR("pole-to-equator ~10008km", dist / 1000.0f, 10008.0f, 50.0f);
  }
}

static void test_sphere_distance_bulk_0() {
  printf("--- sphere_distance: bulk with 0 pairs ---\n");
  pgaccel_status s = pgaccel_sphere_distance_bulk(nullptr, nullptr, 0, true, nullptr, nullptr);
  ASSERT_STATUS_OK("0 pairs OK", s);
}

// ---------------------------------------------------------------------------
// segment_intersects edge cases
// ---------------------------------------------------------------------------

static void test_segment_intersects_collinear() {
  printf("--- segment_intersects: collinear segments ---\n");
  // Collinear overlapping
  {
    double a[] = {0.0, 0.0, 2.0, 0.0};
    double b[] = {1.0, 0.0, 3.0, 0.0};
    int8_t result = 99;
    pgaccel_segment_intersects_bulk(a, b, 1, true, &result);
    ASSERT_EQ("collinear overlapping -> uncertain", result, 0);
  }
  // Collinear non-overlapping
  {
    double a[] = {0.0, 0.0, 1.0, 0.0};
    double b[] = {2.0, 0.0, 3.0, 0.0};
    int8_t result = 99;
    pgaccel_segment_intersects_bulk(a, b, 1, true, &result);
    // Could be uncertain (collinear detection) or definite false
    ASSERT_TRUE("collinear non-overlapping != 1", result != 1);
  }
}

static void test_segment_intersects_endpoint_touch() {
  printf("--- segment_intersects: touching at endpoint ---\n");
  double a[] = {0.0, 0.0, 1.0, 0.0};
  double b[] = {1.0, 0.0, 2.0, 1.0};
  int8_t result = 99;
  pgaccel_segment_intersects_bulk(a, b, 1, true, &result);
  ASSERT_EQ("endpoint touch -> uncertain", result, 0);
}

static void test_segment_intersects_parallel() {
  printf("--- segment_intersects: parallel non-overlapping ---\n");
  double a[] = {0.0, 0.0, 5.0, 0.0};
  double b[] = {0.0, 1.0, 5.0, 1.0};
  int8_t result = 0;
  pgaccel_segment_intersects_bulk(a, b, 1, true, &result);
  ASSERT_EQ("parallel -> no intersect", result, -1);
}

static void test_segment_intersects_perpendicular() {
  printf("--- segment_intersects: perpendicular crossing ---\n");
  double a[] = {0.0, -1.0, 0.0, 1.0};
  double b[] = {-1.0, 0.0, 1.0, 0.0};
  int8_t result = 0;
  pgaccel_segment_intersects_bulk(a, b, 1, true, &result);
  ASSERT_EQ("perpendicular crossing -> intersects", result, 1);
}

static void test_segment_intersects_bulk_0() {
  printf("--- segment_intersects: bulk with 0 segments ---\n");
  pgaccel_status s = pgaccel_segment_intersects_bulk(nullptr, nullptr, 0, true, nullptr);
  ASSERT_STATUS_OK("0 segments OK", s);
}

// =========================================================================
// SPATIAL DISPATCH EDGE CASES
// =========================================================================

static void test_dispatch_0_geoms_a() {
  printf("--- dispatch: 0 geometries in A ---\n");
  float bb[] = {0, 0, 1, 1};
  float c[] = {0.5f, 0.5f};
  pgaccel_geometry b = {PGACCEL_GEOM_POINT, bb, c, 1, nullptr, 0};
  DispatchResult r(1);
  pgaccel_status s =
      test_spatial_cross_product(nullptr, 0, &b, 1, r.dt.data(), &r.dt_count, r.df.data(),
                                 &r.df_count, r.unc.data(), &r.unc_count);
  ASSERT_STATUS_OK("0 geoms A status", s);
  ASSERT_EQ("0 geoms A -> 0 pairs", r.dt_count + r.df_count + r.unc_count, 0);
}

static void test_dispatch_0_geoms_b() {
  printf("--- dispatch: 0 geometries in B ---\n");
  float bb[] = {0, 0, 1, 1};
  float c[] = {0.5f, 0.5f};
  pgaccel_geometry a = {PGACCEL_GEOM_POINT, bb, c, 1, nullptr, 0};
  DispatchResult r(1);
  pgaccel_status s =
      test_spatial_cross_product(&a, 1, nullptr, 0, r.dt.data(), &r.dt_count, r.df.data(),
                                 &r.df_count, r.unc.data(), &r.unc_count);
  ASSERT_STATUS_OK("0 geoms B status", s);
  ASSERT_EQ("0 geoms B -> 0 pairs", r.dt_count + r.df_count + r.unc_count, 0);
}

static void test_dispatch_point_equal() {
  printf("--- dispatch: point vs point (equal within epsilon) ---\n");
  float bb[] = {1.0f, 1.0f, 1.0f, 1.0f};
  float c[] = {1.0f, 1.0f};
  pgaccel_geometry a = {PGACCEL_GEOM_POINT, bb, c, 1, nullptr, 0};
  pgaccel_geometry b = {PGACCEL_GEOM_POINT, bb, c, 1, nullptr, 0};
  DispatchResult r(1);
  pgaccel_status s = test_spatial_cross_product(&a, 1, &b, 1, r.dt.data(), &r.dt_count, r.df.data(),
                                                &r.df_count, r.unc.data(), &r.unc_count);
  ASSERT_STATUS_OK("equal points status", s);
  ASSERT_TRUE("equal points -> exact recheck", has_pair(r.unc.data(), r.unc_count, 0, 0));
}

static void test_dispatch_point_not_equal() {
  printf("--- dispatch: point vs point (not equal) ---\n");
  float bb_a[] = {1, 1, 1, 1};
  float bb_b[] = {5, 5, 5, 5};
  float ca[] = {1.0f, 1.0f};
  float cb[] = {5.0f, 5.0f};
  pgaccel_geometry a = {PGACCEL_GEOM_POINT, bb_a, ca, 1, nullptr, 0};
  pgaccel_geometry b = {PGACCEL_GEOM_POINT, bb_b, cb, 1, nullptr, 0};
  DispatchResult r(1);
  pgaccel_status s = test_spatial_cross_product(&a, 1, &b, 1, r.dt.data(), &r.dt_count, r.df.data(),
                                                &r.df_count, r.unc.data(), &r.unc_count);
  ASSERT_STATUS_OK("unequal points status", s);
  ASSERT_TRUE("unequal points -> definite false", has_pair(r.df.data(), r.df_count, 0, 0));
}

static void test_dispatch_line_crossing() {
  printf("--- dispatch: linestring vs linestring crossing ---\n");
  float ca[] = {0, 0, 2, 2};
  float cb[] = {0, 2, 2, 0};
  float bba[] = {0, 0, 2, 2};
  float bbb[] = {0, 0, 2, 2};
  pgaccel_geometry a = {PGACCEL_GEOM_LINESTRING, bba, ca, 2, nullptr, 0};
  pgaccel_geometry b = {PGACCEL_GEOM_LINESTRING, bbb, cb, 2, nullptr, 0};
  DispatchResult r(1);
  pgaccel_status s = test_spatial_cross_product(&a, 1, &b, 1, r.dt.data(), &r.dt_count, r.df.data(),
                                                &r.df_count, r.unc.data(), &r.unc_count);
  ASSERT_STATUS_OK("crossing lines status", s);
  ASSERT_TRUE("crossing lines -> definite true", has_pair(r.dt.data(), r.dt_count, 0, 0));
}

static void test_dispatch_line_parallel() {
  printf("--- dispatch: linestring vs linestring parallel ---\n");
  float ca[] = {0, 0, 5, 0};
  float cb[] = {0, 3, 5, 3};
  float bba[] = {0, 0, 5, 0};
  float bbb[] = {0, 3, 5, 3};
  pgaccel_geometry a = {PGACCEL_GEOM_LINESTRING, bba, ca, 2, nullptr, 0};
  pgaccel_geometry b = {PGACCEL_GEOM_LINESTRING, bbb, cb, 2, nullptr, 0};
  DispatchResult r(1);
  pgaccel_status s = test_spatial_cross_product(&a, 1, &b, 1, r.dt.data(), &r.dt_count, r.df.data(),
                                                &r.df_count, r.unc.data(), &r.unc_count);
  ASSERT_STATUS_OK("parallel lines status", s);
  // Bbox miss: definite false
  ASSERT_TRUE("parallel lines -> definite false", has_pair(r.df.data(), r.df_count, 0, 0));
}

static void test_dispatch_polygon_with_hole_point_in_hole() {
  printf("--- dispatch: polygon with hole, point in hole ---\n");
  // Outer ring: (0,0)-(10,0)-(10,10)-(0,10)-(0,0)
  float poly_coords[] = {
      0, 0, 10, 0, 10, 10, 0, 10, 0, 0,  // outer (5 coord pairs)
      3, 3, 7,  3, 7,  7,  3, 7,  3, 3,  // hole (5 coord pairs)
  };
  float poly_bbox[] = {0, 0, 10, 10};
  uint32_t ring_offsets[] = {0, 5};  // outer starts at 0, hole at 5
  pgaccel_geometry polygon = {PGACCEL_GEOM_POLYGON, poly_bbox, poly_coords, 10, ring_offsets, 2};

  // Point in hole (5, 5)
  float pt_c[] = {5.0f, 5.0f};
  float pt_bb[] = {5, 5, 5, 5};
  pgaccel_geometry pt = {PGACCEL_GEOM_POINT, pt_bb, pt_c, 1, nullptr, 0};

  DispatchResult r(1);
  pgaccel_status s =
      test_spatial_cross_product(&pt, 1, &polygon, 1, r.dt.data(), &r.dt_count, r.df.data(),
                                 &r.df_count, r.unc.data(), &r.unc_count);
  ASSERT_STATUS_OK("point in hole status", s);
  // Should be false or uncertain (hole cancels containment)
  ASSERT_TRUE("point in hole -> not definite true", !has_pair(r.dt.data(), r.dt_count, 0, 0));
}

static void test_dispatch_polygon_with_hole_point_in_shell() {
  printf("--- dispatch: polygon with hole, point in shell ---\n");
  float poly_coords[] = {
      0, 0, 10, 0, 10, 10, 0, 10, 0, 0, 3, 3, 7, 3, 7, 7, 3, 7, 3, 3,
  };
  float poly_bbox[] = {0, 0, 10, 10};
  uint32_t ring_offsets[] = {0, 5};
  pgaccel_geometry polygon = {PGACCEL_GEOM_POLYGON, poly_bbox, poly_coords, 10, ring_offsets, 2};

  // Point in shell (1.5, 1.5) - inside outer, outside hole
  float pt_c[] = {1.5f, 1.5f};
  float pt_bb[] = {1.5f, 1.5f, 1.5f, 1.5f};
  pgaccel_geometry pt = {PGACCEL_GEOM_POINT, pt_bb, pt_c, 1, nullptr, 0};

  DispatchResult r(1);
  pgaccel_status s =
      test_spatial_cross_product(&pt, 1, &polygon, 1, r.dt.data(), &r.dt_count, r.df.data(),
                                 &r.df_count, r.unc.data(), &r.unc_count);
  ASSERT_STATUS_OK("point in shell status", s);
  // Should be true or uncertain (inside outer, outside hole)
  ASSERT_TRUE("point in shell -> not definite false", !has_pair(r.df.data(), r.df_count, 0, 0));
}

static void test_dispatch_unknown_geom_type() {
  printf("--- dispatch: unknown geometry type ---\n");
  float bb[] = {0, 0, 1, 1};
  float c[] = {0.5f, 0.5f};
  pgaccel_geometry a = {PGACCEL_GEOM_UNKNOWN, bb, c, 1, nullptr, 0};
  pgaccel_geometry b = {PGACCEL_GEOM_POINT, bb, c, 1, nullptr, 0};
  DispatchResult r(1);
  pgaccel_status s = test_spatial_cross_product(&a, 1, &b, 1, r.dt.data(), &r.dt_count, r.df.data(),
                                                &r.df_count, r.unc.data(), &r.unc_count);
  ASSERT_STATUS_OK("unknown type status", s);
  // Bbox overlaps so not filtered, but unknown type -> uncertain
  ASSERT_EQ("unknown type -> uncertain", r.unc_count, 1);
  ASSERT_EQ("unknown type -> 0 definite true", r.dt_count, 0);
}

static void test_dispatch_null_bbox() {
  printf("--- dispatch: geometry with NULL bbox ---\n");
  float c[] = {0.5f, 0.5f};
  pgaccel_geometry a = {PGACCEL_GEOM_POINT, nullptr, c, 1, nullptr, 0};
  float bb[] = {0, 0, 1, 1};
  pgaccel_geometry b = {PGACCEL_GEOM_POINT, bb, c, 1, nullptr, 0};
  DispatchResult r(1);
  pgaccel_status s = test_spatial_cross_product(&a, 1, &b, 1, r.dt.data(), &r.dt_count, r.df.data(),
                                                &r.df_count, r.unc.data(), &r.unc_count);
  // Should not crash; result is uncertain or an error
  ASSERT_TRUE("null bbox no crash", s == PGACCEL_OK || s == PGACCEL_ERROR);
}

// =========================================================================
// H3 EDGE CASES
// =========================================================================

static void test_h3_resolution_0_to_15() {
  printf("--- h3: get_resolution 0 through 15 ---\n");
  int digits[15] = {0};
  for (int res = 0; res <= 15; res++) {
    uint64_t cell = make_cell(0, res, digits);
    int32_t result = -1;
    pgaccel_status s = pgaccel_h3_get_resolution_bulk(&cell, 1, &result);
    ASSERT_STATUS_OK("get_resolution status", s);
    char desc[64];
    snprintf(desc, sizeof(desc), "resolution %d", res);
    ASSERT_EQ(desc, result, res);
  }
}

static void test_h3_parent_same_res() {
  printf("--- h3: cell_to_parent same resolution ---\n");
  int digits[15] = {1, 2, 3, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0};
  uint64_t cell = make_cell(5, 3, digits);
  uint64_t parent = 0;
  pgaccel_status s = pgaccel_h3_cell_to_parent_bulk(&cell, 1, 3, &parent);
  ASSERT_STATUS_OK("parent same res status", s);
  ASSERT_EQ("parent same res = identity", parent, cell);
}

static void test_h3_parent_res_0() {
  printf("--- h3: cell_to_parent resolution 0 ---\n");
  int digits[15] = {1, 2, 3, 4, 5, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0};
  uint64_t cell = make_cell(10, 5, digits);
  uint64_t parent = 0;
  pgaccel_status s = pgaccel_h3_cell_to_parent_bulk(&cell, 1, 0, &parent);
  ASSERT_STATUS_OK("parent res 0 status", s);
  int base_digits[15] = {0};
  uint64_t expected = make_cell(10, 0, base_digits);
  ASSERT_EQ("parent res 0", parent, expected);
}

static void test_h3_grid_distance_same_cell() {
  printf("--- h3: grid_distance same cell ---\n");
  int digits[15] = {1, 2, 3, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0};
  uint64_t cell = make_cell(5, 3, digits);
  int32_t dist = -99;
  pgaccel_status s = pgaccel_h3_grid_distance_bulk(&cell, &cell, 1, &dist);
  ASSERT_STATUS_OK("same cell distance status", s);
  ASSERT_EQ("same cell distance = 0", dist, 0);
}

static void test_h3_grid_distance_adjacent() {
  printf("--- h3: grid_distance adjacent cells ---\n");
  int da[15] = {0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0};
  int db[15] = {1, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0};
  uint64_t ca = make_cell(5, 1, da);
  uint64_t cb = make_cell(5, 1, db);
  int32_t dist = -99;
  pgaccel_status s = pgaccel_h3_grid_distance_bulk(&ca, &cb, 1, &dist);
  ASSERT_STATUS_OK("adjacent cells status", s);
  ASSERT_TRUE("adjacent cells distance > 0", dist > 0);
}

static void test_h3_lat_lng_poles() {
  printf("--- h3: lat_lng_to_cell at poles ---\n");
  // North pole
  {
    double lat = 90.0, lng = 0.0;
    uint64_t cell = 0;
    uint8_t valid = 0;
    pgaccel_status s = pgaccel_h3_lat_lng_to_cell_bulk(&lat, &lng, 1, 2, true, &cell, &valid);
    ASSERT_STATUS_OK("north pole status", s);
    // No crash; validity depends on implementation
  }
  // South pole
  {
    double lat = -90.0, lng = 0.0;
    uint64_t cell = 0;
    uint8_t valid = 0;
    pgaccel_status s = pgaccel_h3_lat_lng_to_cell_bulk(&lat, &lng, 1, 2, true, &cell, &valid);
    ASSERT_STATUS_OK("south pole status", s);
  }
}

static void test_h3_lat_lng_antimeridian() {
  printf("--- h3: lat_lng_to_cell at antimeridian ---\n");
  {
    double lat = 0.0, lng = 180.0;
    uint64_t cell = 0;
    uint8_t valid = 0;
    pgaccel_status s = pgaccel_h3_lat_lng_to_cell_bulk(&lat, &lng, 1, 5, true, &cell, &valid);
    ASSERT_STATUS_OK("lon=180 status", s);
  }
  {
    double lat = 0.0, lng = -180.0;
    uint64_t cell = 0;
    uint8_t valid = 0;
    pgaccel_status s = pgaccel_h3_lat_lng_to_cell_bulk(&lat, &lng, 1, 5, true, &cell, &valid);
    ASSERT_STATUS_OK("lon=-180 status", s);
  }
}

static void test_h3_lat_lng_origin() {
  printf("--- h3: lat_lng_to_cell at origin ---\n");
  double lat = 0.0, lng = 0.0;
  uint64_t cell = 0;
  uint8_t valid = 0;
  pgaccel_status s = pgaccel_h3_lat_lng_to_cell_bulk(&lat, &lng, 1, 5, true, &cell, &valid);
  ASSERT_STATUS_OK("origin status", s);
  ASSERT_EQ("origin valid", valid, 1);
  ASSERT_TRUE("origin cell non-zero", cell != 0);

  // Verify resolution
  int32_t res = -1;
  pgaccel_h3_get_resolution_bulk(&cell, 1, &res);
  ASSERT_EQ("origin cell res = 5", res, 5);
}

static void test_h3_lat_lng_bulk_0() {
  printf("--- h3: lat_lng_to_cell bulk with 0 elements ---\n");
  pgaccel_status s =
      pgaccel_h3_lat_lng_to_cell_bulk(nullptr, nullptr, 0, 5, true, nullptr, nullptr);
  ASSERT_STATUS_OK("0 elements OK", s);
}

// =========================================================================
// RASTER EDGE CASES
// =========================================================================

static void test_map_algebra_empty_expr() {
  printf("--- raster: map_algebra empty expression (0 instructions) ---\n");
  float band[] = {1.0f, 2.0f, 3.0f, 4.0f};
  const void* bands[] = {band};
  pgaccel_expr expr;
  expr.instructions = nullptr;
  expr.inst_count = 0;
  expr.band_count = 1;
  float output[4] = {0};
  uint8_t nodata[4] = {0};
  pgaccel_status s = pgaccel_map_algebra(bands, 4, PGACCEL_PT_FLOAT32, &expr, output, nodata);
  // Empty expression: should succeed or return error, but not crash
  ASSERT_TRUE("empty expr no crash", s == PGACCEL_OK || s == PGACCEL_ERROR);
}

static void test_map_algebra_div_zero() {
  printf("--- raster: map_algebra division by zero ---\n");
  float band0[] = {10.0f, 20.0f};
  float band1[] = {0.0f, 0.0f};
  const void* bands[] = {band0, band1};
  pgaccel_expr_inst code[] = {
      make_load_band(0),
      make_load_band(1),
      make_op(PGACCEL_OP_DIV),
  };
  pgaccel_expr expr;
  expr.instructions = code;
  expr.inst_count = 3;
  expr.band_count = 2;
  float output[2] = {0};
  uint8_t nodata[2] = {0};
  pgaccel_status s = pgaccel_map_algebra(bands, 2, PGACCEL_PT_FLOAT32, &expr, output, nodata);
  ASSERT_STATUS_OK("div zero status", s);
  // Division by zero should mark as nodata
  ASSERT_EQ("div zero [0] -> nodata", nodata[0], 1);
  ASSERT_EQ("div zero [1] -> nodata", nodata[1], 1);
}

static void test_map_algebra_sqrt_negative() {
  printf("--- raster: map_algebra sqrt of negative number ---\n");
  float band[] = {-4.0f, -1.0f, 4.0f, 0.0f};
  const void* bands[] = {band};
  pgaccel_expr_inst code[] = {
      make_load_band(0),
      make_op(PGACCEL_OP_SQRT),
  };
  pgaccel_expr expr;
  expr.instructions = code;
  expr.inst_count = 2;
  expr.band_count = 1;
  float output[4] = {0};
  uint8_t nodata[4] = {0};
  pgaccel_status s = pgaccel_map_algebra(bands, 4, PGACCEL_PT_FLOAT32, &expr, output, nodata);
  ASSERT_STATUS_OK("sqrt neg status", s);
  // sqrt(-4) and sqrt(-1) should produce NaN -> nodata
  ASSERT_EQ("sqrt(-4) -> nodata", nodata[0], 1);
  ASSERT_EQ("sqrt(-1) -> nodata", nodata[1], 1);
  // sqrt(4) = 2, sqrt(0) = 0
  ASSERT_EQ("sqrt(4) not nodata", nodata[2], 0);
  ASSERT_NEAR("sqrt(4) = 2", output[2], 2.0f, 0.01f);
  ASSERT_EQ("sqrt(0) not nodata", nodata[3], 0);
  ASSERT_NEAR("sqrt(0) = 0", output[3], 0.0f, 0.01f);
}

static void test_map_algebra_log_zero() {
  printf("--- raster: map_algebra log of zero ---\n");
  float band[] = {0.0f, 1.0f, 10.0f, -1.0f};
  const void* bands[] = {band};
  pgaccel_expr_inst code[] = {
      make_load_band(0),
      make_op(PGACCEL_OP_LOG),
  };
  pgaccel_expr expr;
  expr.instructions = code;
  expr.inst_count = 2;
  expr.band_count = 1;
  float output[4] = {0};
  uint8_t nodata[4] = {0};
  pgaccel_status s = pgaccel_map_algebra(bands, 4, PGACCEL_PT_FLOAT32, &expr, output, nodata);
  ASSERT_STATUS_OK("log zero status", s);
  // log(0) = -inf -> nodata
  ASSERT_EQ("log(0) -> nodata", nodata[0], 1);
  // log(1) = 0
  ASSERT_EQ("log(1) not nodata", nodata[1], 0);
  ASSERT_NEAR("log(1) = 0", output[1], 0.0f, 0.01f);
  // log(10) ~ 2.302585
  ASSERT_EQ("log(10) not nodata", nodata[2], 0);
  ASSERT_NEAR("log(10) ~ 2.303", output[2], 2.302585f, 0.01f);
  // log(-1) = NaN -> nodata
  ASSERT_EQ("log(-1) -> nodata", nodata[3], 1);
}

static void test_map_algebra_0_pixels() {
  printf("--- raster: map_algebra 0 pixels ---\n");
  float dummy = 0;
  const void* bands[] = {&dummy};
  pgaccel_expr_inst code[] = {make_load_band(0)};
  pgaccel_expr expr;
  expr.instructions = code;
  expr.inst_count = 1;
  expr.band_count = 1;
  float out = 0;
  uint8_t nd = 0;
  pgaccel_status s = pgaccel_map_algebra(bands, 0, PGACCEL_PT_FLOAT32, &expr, &out, &nd);
  ASSERT_STATUS_OK("0 pixels status", s);
}

static void test_raster_clip_0_vertices() {
  printf("--- raster: clip ring with 0 vertices ---\n");
  float pixel = 42.0f;
  float out = 0;
  uint8_t nd = 0;
  pgaccel_status s =
      pgaccel_raster_clip(&pixel, 1, 1, 0, 0, 1, 1, PGACCEL_PT_FLOAT32, nullptr, 0, &out, &nd);
  // Should handle gracefully
  ASSERT_TRUE("clip 0 vertices no crash", s == PGACCEL_OK || s == PGACCEL_ERROR);
}

static void test_raster_clip_triangle() {
  printf("--- raster: clip ring with 3 vertices (triangle) ---\n");
  const size_t W = 10, H = 10;
  std::vector<float> input(W * H, 1.0f);
  std::vector<float> output(W * H, 0.0f);
  std::vector<uint8_t> nodata(W * H, 0);
  float ring[] = {5.0f, 0.0f, 10.0f, 10.0f, 0.0f, 10.0f};
  pgaccel_status s = pgaccel_raster_clip(input.data(), W, H, 0, 0, 1, 1, PGACCEL_PT_FLOAT32, ring,
                                         3, output.data(), nodata.data());
  ASSERT_STATUS_OK("clip triangle status", s);
  // Some pixels inside, some outside
  int inside = 0;
  for (size_t i = 0; i < W * H; i++) {
    if (nodata[i] == 0)
      inside++;
  }
  ASSERT_TRUE("triangle clip: some inside", inside > 0);
  ASSERT_TRUE("triangle clip: some outside", inside < (int)(W * H));
}

static void test_raster_clip_0x0() {
  printf("--- raster: clip 0x0 raster ---\n");
  float ring[] = {0, 0, 1, 0, 1, 1, 0, 1};
  float dummy = 0;
  uint8_t nd = 0;
  pgaccel_status s =
      pgaccel_raster_clip(&dummy, 0, 0, 0, 0, 1, 1, PGACCEL_PT_FLOAT32, ring, 4, &dummy, &nd);
  ASSERT_STATUS_OK("clip 0x0 status", s);
}

static void test_raster_clip_1x1() {
  printf("--- raster: clip 1x1 raster ---\n");
  float pixel = 42.0f;
  float output = 0;
  uint8_t nodata = 0;
  // Ring that covers the pixel
  float ring[] = {-1, -1, 2, -1, 2, 2, -1, 2};
  pgaccel_status s =
      pgaccel_raster_clip(&pixel, 1, 1, 0, 0, 1, 1, PGACCEL_PT_FLOAT32, ring, 4, &output, &nodata);
  ASSERT_STATUS_OK("clip 1x1 status", s);
  ASSERT_EQ("1x1 inside -> not nodata", nodata, 0);
  ASSERT_NEAR("1x1 value preserved", output, 42.0f, 0.01f);
}

static void test_reclass_0_rules() {
  printf("--- raster: reclass 0 rules ---\n");
  float input[] = {1.0f, 2.0f, 3.0f};
  float output[3] = {0};
  pgaccel_status s =
      pgaccel_raster_reclass(input, 3, PGACCEL_PT_FLOAT32, nullptr, 0, PGACCEL_PT_FLOAT32, output);
  ASSERT_STATUS_OK("reclass 0 rules status", s);
  // All values should pass through
  ASSERT_NEAR("reclass passthrough [0]", output[0], 1.0f, 0.01f);
  ASSERT_NEAR("reclass passthrough [1]", output[1], 2.0f, 0.01f);
  ASSERT_NEAR("reclass passthrough [2]", output[2], 3.0f, 0.01f);
}

static void test_reclass_overlapping_rules() {
  printf("--- raster: reclass overlapping rules ---\n");
  float input[] = {5.0f, 15.0f, 25.0f};
  pgaccel_reclass_rule rules[] = {
      {0.0, 20.0, 100.0},   // [0, 20) -> 100
      {10.0, 30.0, 200.0},  // [10, 30) -> 200 (overlaps)
  };
  float output[3] = {0};
  pgaccel_status s =
      pgaccel_raster_reclass(input, 3, PGACCEL_PT_FLOAT32, rules, 2, PGACCEL_PT_FLOAT32, output);
  ASSERT_STATUS_OK("overlapping rules status", s);
  // 5 matches rule 0 -> 100
  ASSERT_NEAR("overlap: 5 -> 100", output[0], 100.0f, 0.01f);
  // 15 matches both rules; first-match wins
  ASSERT_NEAR("overlap: 15 -> first match (100)", output[1], 100.0f, 0.01f);
  // 25 matches only rule 1 -> 200
  ASSERT_NEAR("overlap: 25 -> 200", output[2], 200.0f, 0.01f);
}

static void test_reclass_no_match_passthrough() {
  printf("--- raster: reclass no matching rule (passthrough) ---\n");
  float input[] = {-10.0f, 999.0f, 50.0f};
  pgaccel_reclass_rule rules[] = {
      {0.0, 100.0, 1.0},
  };
  float output[3] = {0};
  pgaccel_status s =
      pgaccel_raster_reclass(input, 3, PGACCEL_PT_FLOAT32, rules, 1, PGACCEL_PT_FLOAT32, output);
  ASSERT_STATUS_OK("no match status", s);
  ASSERT_NEAR("passthrough -10", output[0], -10.0f, 0.01f);
  ASSERT_NEAR("passthrough 999", output[1], 999.0f, 0.01f);
  ASSERT_NEAR("matched 50 -> 1", output[2], 1.0f, 0.01f);
}

// =========================================================================
// BBOX EDGE CASES
// =========================================================================

static void test_bbox_identical_boxes() {
  printf("--- bbox: identical boxes ---\n");
  float a[] = {1.0f, 2.0f, 3.0f, 4.0f};
  float b[] = {1.0f, 2.0f, 3.0f, 4.0f};
  uint8_t result = 0;
  size_t hits = 0;
  pgaccel_status s = pgaccel_bbox_intersects_bulk_f32(a, 1, b, 1, &result, &hits);
  ASSERT_STATUS_OK("identical bbox status", s);
  ASSERT_EQ("identical bbox -> intersects", result, 1);
  ASSERT_EQ("identical bbox hit count", hits, 1);
}

static void test_bbox_touching_edge() {
  printf("--- bbox: touching at edge ---\n");
  float a[] = {0, 0, 1, 1};
  float b[] = {1, 0, 2, 1};  // shares x=1 edge
  uint8_t result = 0;
  size_t hits = 0;
  pgaccel_status s = pgaccel_bbox_intersects_bulk_f32(a, 1, b, 1, &result, &hits);
  ASSERT_STATUS_OK("edge touch status", s);
  // Touching at edge: intersects (closed intervals)
  ASSERT_EQ("edge touch -> intersects", result, 1);
}

static void test_bbox_touching_corner() {
  printf("--- bbox: touching at corner ---\n");
  float a[] = {0, 0, 1, 1};
  float b[] = {1, 1, 2, 2};  // shares corner (1,1)
  uint8_t result = 0;
  size_t hits = 0;
  pgaccel_status s = pgaccel_bbox_intersects_bulk_f32(a, 1, b, 1, &result, &hits);
  ASSERT_STATUS_OK("corner touch status", s);
  // Corner touch: intersects (closed intervals)
  ASSERT_EQ("corner touch -> intersects", result, 1);
}

static void test_bbox_zero_area_point() {
  printf("--- bbox: zero-area box (point) ---\n");
  float a[] = {5, 5, 5, 5};  // point box
  float b[] = {4, 4, 6, 6};  // box containing the point
  uint8_t result = 0;
  size_t hits = 0;
  pgaccel_status s = pgaccel_bbox_intersects_bulk_f32(a, 1, b, 1, &result, &hits);
  ASSERT_STATUS_OK("point box status", s);
  ASSERT_EQ("point box inside -> intersects", result, 1);

  // Point box outside
  float c[] = {10, 10, 10, 10};
  result = 0;
  hits = 0;
  s = pgaccel_bbox_intersects_bulk_f32(a, 1, c, 1, &result, &hits);
  ASSERT_STATUS_OK("point box outside status", s);
  ASSERT_EQ("point box outside -> no intersect", result, 0);
}

static void test_bbox_0_elements_a() {
  printf("--- bbox: 0 elements in A ---\n");
  float b[] = {0, 0, 1, 1};
  size_t hits = 99;
  pgaccel_status s = pgaccel_bbox_intersects_bulk_f32(nullptr, 0, b, 1, nullptr, &hits);
  ASSERT_STATUS_OK("0 elems A status", s);
  ASSERT_EQ("0 elems A -> 0 hits", hits, 0);
}

static void test_bbox_0_elements_b() {
  printf("--- bbox: 0 elements in B ---\n");
  float a[] = {0, 0, 1, 1};
  size_t hits = 99;
  pgaccel_status s = pgaccel_bbox_intersects_bulk_f32(a, 1, nullptr, 0, nullptr, &hits);
  ASSERT_STATUS_OK("0 elems B status", s);
  ASSERT_EQ("0 elems B -> 0 hits", hits, 0);
}

static void test_bbox_large_batch() {
  printf("--- bbox: large batch (100x100) ---\n");
  const size_t N = 100;
  // Create N non-overlapping boxes on A side, one large box on B side
  std::vector<float> a(N * 4);
  for (size_t i = 0; i < N; i++) {
    float x = (float)(i * 10);
    a[i * 4 + 0] = x;
    a[i * 4 + 1] = 0;
    a[i * 4 + 2] = x + 5;
    a[i * 4 + 3] = 5;
  }
  // B: one box that covers first 50 A boxes
  float b[] = {0, 0, 495, 5};
  std::vector<uint8_t> result(N, 0);
  size_t hits = 0;
  pgaccel_status s = pgaccel_bbox_intersects_bulk_f32(a.data(), N, b, 1, result.data(), &hits);
  ASSERT_STATUS_OK("large batch status", s);
  ASSERT_EQ("large batch hits = 50", hits, 50);
  for (size_t i = 0; i < 50; i++) {
    ASSERT_EQ("first 50 intersect", result[i], 1);
  }
  for (size_t i = 50; i < N; i++) {
    ASSERT_EQ("last 50 no intersect", result[i], 0);
  }
}

static void test_bbox_separated() {
  printf("--- bbox: fully separated boxes ---\n");
  float a[] = {0, 0, 1, 1};
  float b[] = {10, 10, 20, 20};
  uint8_t result = 99;
  size_t hits = 99;
  pgaccel_status s = pgaccel_bbox_intersects_bulk_f32(a, 1, b, 1, &result, &hits);
  ASSERT_STATUS_OK("separated status", s);
  ASSERT_EQ("separated -> no intersect", result, 0);
  ASSERT_EQ("separated -> 0 hits", hits, 0);
}

static void test_bbox_nested() {
  printf("--- bbox: one box inside another ---\n");
  float a[] = {0, 0, 10, 10};
  float b[] = {2, 2, 5, 5};
  uint8_t result = 0;
  size_t hits = 0;
  pgaccel_status s = pgaccel_bbox_intersects_bulk_f32(a, 1, b, 1, &result, &hits);
  ASSERT_STATUS_OK("nested status", s);
  ASSERT_EQ("nested -> intersects", result, 1);
  ASSERT_EQ("nested -> 1 hit", hits, 1);
}

// =========================================================================
// ADDITIONAL SPATIAL PREDICATE STRESS TESTS
// =========================================================================

static void test_point_in_ring_concave_polygon() {
  printf("--- point_in_ring: concave L-shaped polygon ---\n");
  // L-shaped: (0,0)-(4,0)-(4,2)-(2,2)-(2,4)-(0,4)-(0,0)
  double ring[] = {
      0, 0, 4, 0, 4, 2, 2, 2, 2, 4, 0, 4, 0, 0,
  };
  // Inside the bottom of the L
  {
    double pt[] = {3.0, 1.0};
    int8_t result = 0;
    pgaccel_point_in_ring_bulk(pt, 1, ring, 7, true, &result);
    ASSERT_EQ("L bottom inside", result, 1);
  }
  // Inside the left arm of the L
  {
    double pt[] = {1.0, 3.0};
    int8_t result = 0;
    pgaccel_point_in_ring_bulk(pt, 1, ring, 7, true, &result);
    ASSERT_EQ("L left arm inside", result, 1);
  }
  // In the concavity (top-right) — outside
  {
    double pt[] = {3.0, 3.0};
    int8_t result = 0;
    pgaccel_point_in_ring_bulk(pt, 1, ring, 7, true, &result);
    ASSERT_EQ("L concavity outside", result, -1);
  }
}

static void test_point_in_ring_fp32_near_edge() {
  printf("--- point_in_ring: fp32 near edge ---\n");
  float ring[] = {0, 0, 1, 0, 1, 1, 0, 1, 0, 0};
  // Point very near edge in fp32 -> uncertain
  float pt[] = {0.5f, 1e-7f};
  int8_t result = 99;
  pgaccel_point_in_ring_bulk(pt, 1, ring, 5, false, &result);
  ASSERT_EQ("fp32 near edge -> uncertain", result, 0);
}

static void test_sphere_distance_fp32_close() {
  printf("--- sphere_distance: fp32 very close points ---\n");
  float a[] = {0.0f, 0.0f};
  float b[] = {0.0000001f, 0.0f};
  float dist = 0;
  uint8_t unc = 0;
  pgaccel_sphere_distance_bulk(a, b, 1, false, &dist, &unc);
  ASSERT_EQ("fp32 very close -> uncertain", unc, 1);
}

static void test_sphere_distance_batch_mixed() {
  printf("--- sphere_distance: batch with mixed definite/uncertain ---\n");
  float a[] = {
      -74.006f, 40.7128f,  // NYC
      0.0f,     0.0f,      // origin
      0.0f,     90.0f,     // north pole
  };
  float b[] = {
      -0.1278f, 51.5074f,  // London
      0.0f,     0.0f,      // same as origin -> uncertain
      0.0f,     -90.0f,    // south pole -> antipodal -> uncertain
  };
  float dists[3] = {0};
  uint8_t uncs[3] = {0};
  pgaccel_status s = pgaccel_sphere_distance_bulk(a, b, 3, false, dists, uncs);
  ASSERT_STATUS_OK("batch mixed status", s);
  ASSERT_EQ("NYC-London definite", uncs[0], 0);
  ASSERT_EQ("same point uncertain", uncs[1], 1);
  ASSERT_EQ("pole-to-pole uncertain", uncs[2], 1);
  ASSERT_NEAR("NYC-London ~5570km", dists[0] / 1000.0f, 5570.0f, 50.0f);
}

static void test_segment_intersects_T_junction() {
  printf("--- segment_intersects: T-junction ---\n");
  // Horizontal segment (0,0)-(2,0), vertical segment (1,-1)-(1,0)
  // Touches at (1,0) endpoint
  double a[] = {0.0, 0.0, 2.0, 0.0};
  double b[] = {1.0, -1.0, 1.0, 0.0};
  int8_t result = 99;
  pgaccel_segment_intersects_bulk(a, b, 1, true, &result);
  // Endpoint on segment -> uncertain
  ASSERT_EQ("T-junction -> uncertain", result, 0);
}

static void test_segment_intersects_L_shape() {
  printf("--- segment_intersects: L-shaped endpoint ---\n");
  // Two segments sharing an endpoint making an L
  double a[] = {0.0, 0.0, 1.0, 0.0};
  double b[] = {1.0, 0.0, 1.0, 1.0};
  int8_t result = 99;
  pgaccel_segment_intersects_bulk(a, b, 1, true, &result);
  ASSERT_EQ("L endpoint -> uncertain", result, 0);
}

static void test_segment_intersects_nearly_parallel() {
  printf("--- segment_intersects: nearly parallel ---\n");
  double a[] = {0.0, 0.0, 10.0, 0.0};
  double b[] = {0.0, 0.001, 10.0, 0.001};
  int8_t result = 0;
  pgaccel_segment_intersects_bulk(a, b, 1, true, &result);
  ASSERT_EQ("nearly parallel -> no intersect", result, -1);
}

// =========================================================================
// ADDITIONAL H3 EDGE CASES
// =========================================================================

static void test_h3_lat_lng_fp32_low_res() {
  printf("--- h3: lat_lng_to_cell fp32 at low resolution ---\n");
  // fp32 should be accurate enough for low resolution
  float lat = 40.689247f, lng = -74.044502f;
  uint64_t cell = 0;
  uint8_t valid = 0;
  pgaccel_status s = pgaccel_h3_lat_lng_to_cell_bulk(&lat, &lng, 1, 3, false, &cell, &valid);
  ASSERT_STATUS_OK("fp32 low res status", s);
  // At low res, fp32 should produce a valid cell
  if (valid) {
    int32_t res = -1;
    pgaccel_h3_get_resolution_bulk(&cell, 1, &res);
    ASSERT_EQ("fp32 cell res = 3", res, 3);
  }
}

static void test_h3_lat_lng_fp32_high_res_exact_fixup() {
  printf("--- h3: lat_lng_to_cell fp32 at high resolution ---\n");
  float lat = 0.0f, lng = 0.0f;
  uint64_t cell = 0;
  uint8_t valid = 0;
  pgaccel_status s = pgaccel_h3_lat_lng_to_cell_bulk(&lat, &lng, 1, 12, false, &cell, &valid);
  ASSERT_STATUS_OK("fp32 high res status", s);
  ASSERT_EQ("fp32 high res exact fixup -> valid", valid, 1);
  int32_t res = -1;
  pgaccel_h3_get_resolution_bulk(&cell, 1, &res);
  ASSERT_EQ("fp32 high res exact fixup cell res = 12", res, 12);
}

static void test_h3_get_resolution_invalid_cell() {
  printf("--- h3: get_resolution on invalid cell ---\n");
  uint64_t zero = 0;
  int32_t res = 99;
  pgaccel_status s = pgaccel_h3_get_resolution_bulk(&zero, 1, &res);
  ASSERT_STATUS_OK("invalid cell status", s);
  ASSERT_EQ("invalid cell res = -1", res, -1);
}

static void test_h3_grid_distance_diff_resolution() {
  printf("--- h3: grid_distance different resolutions ---\n");
  int d1[15] = {1, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0};
  int d3[15] = {1, 2, 3, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0};
  uint64_t c1 = make_cell(5, 1, d1);
  uint64_t c3 = make_cell(5, 3, d3);
  int32_t dist = 99;
  pgaccel_status s = pgaccel_h3_grid_distance_bulk(&c1, &c3, 1, &dist);
  ASSERT_STATUS_OK("diff res status", s);
  ASSERT_EQ("diff res -> -1", dist, -1);
}

static void test_h3_cell_to_parent_higher_res() {
  printf("--- h3: cell_to_parent higher resolution ---\n");
  int digits[15] = {1, 2, 3, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0};
  uint64_t cell = make_cell(5, 3, digits);
  uint64_t parent = 99;
  pgaccel_status s = pgaccel_h3_cell_to_parent_bulk(&cell, 1, 5, &parent);
  ASSERT_STATUS_OK("higher res parent status", s);
  // Parent at higher res than cell -> returns 0
  ASSERT_EQ("higher res parent -> 0", parent, 0ULL);
}

// =========================================================================
// ADDITIONAL RASTER EDGE CASES
// =========================================================================

static void test_map_algebra_abs() {
  printf("--- raster: map_algebra abs ---\n");
  float band[] = {-5.0f, 3.0f, 0.0f, -100.0f};
  const void* bands[] = {band};
  pgaccel_expr_inst code[] = {
      make_load_band(0),
      make_op(PGACCEL_OP_ABS),
  };
  pgaccel_expr expr;
  expr.instructions = code;
  expr.inst_count = 2;
  expr.band_count = 1;
  float output[4] = {0};
  uint8_t nodata[4] = {0};
  pgaccel_status s = pgaccel_map_algebra(bands, 4, PGACCEL_PT_FLOAT32, &expr, output, nodata);
  ASSERT_STATUS_OK("abs status", s);
  ASSERT_NEAR("abs(-5) = 5", output[0], 5.0f, 0.01f);
  ASSERT_NEAR("abs(3) = 3", output[1], 3.0f, 0.01f);
  ASSERT_NEAR("abs(0) = 0", output[2], 0.0f, 0.01f);
  ASSERT_NEAR("abs(-100) = 100", output[3], 100.0f, 0.01f);
}

static void test_map_algebra_pow() {
  printf("--- raster: map_algebra pow ---\n");
  float band[] = {2.0f, 3.0f, 10.0f};
  const void* bands[] = {band};
  pgaccel_expr_inst code[] = {
      make_load_band(0),
      make_load_const(3.0),
      make_op(PGACCEL_OP_POW),
  };
  pgaccel_expr expr;
  expr.instructions = code;
  expr.inst_count = 3;
  expr.band_count = 1;
  float output[3] = {0};
  uint8_t nodata[3] = {0};
  pgaccel_status s = pgaccel_map_algebra(bands, 3, PGACCEL_PT_FLOAT32, &expr, output, nodata);
  ASSERT_STATUS_OK("pow status", s);
  ASSERT_NEAR("2^3 = 8", output[0], 8.0f, 0.01f);
  ASSERT_NEAR("3^3 = 27", output[1], 27.0f, 0.01f);
  ASSERT_NEAR("10^3 = 1000", output[2], 1000.0f, 0.5f);
}

static void test_map_algebra_comparison() {
  printf("--- raster: map_algebra GT/LT/EQ ---\n");
  float band[] = {1.0f, 5.0f, 10.0f};
  const void* bands[] = {band};

  // band[0] > 5 -> {0, 0, 1}
  {
    pgaccel_expr_inst code[] = {
        make_load_band(0),
        make_load_const(5.0),
        make_op(PGACCEL_OP_GT),
    };
    pgaccel_expr expr;
    expr.instructions = code;
    expr.inst_count = 3;
    expr.band_count = 1;
    float output[3] = {0};
    uint8_t nodata[3] = {0};
    pgaccel_status s = pgaccel_map_algebra(bands, 3, PGACCEL_PT_FLOAT32, &expr, output, nodata);
    ASSERT_STATUS_OK("GT status", s);
    ASSERT_NEAR("1 > 5 = 0", output[0], 0.0f, 0.01f);
    ASSERT_NEAR("5 > 5 = 0", output[1], 0.0f, 0.01f);
    ASSERT_NEAR("10 > 5 = 1", output[2], 1.0f, 0.01f);
  }

  // band[0] < 5 -> {1, 0, 0}
  {
    pgaccel_expr_inst code[] = {
        make_load_band(0),
        make_load_const(5.0),
        make_op(PGACCEL_OP_LT),
    };
    pgaccel_expr expr;
    expr.instructions = code;
    expr.inst_count = 3;
    expr.band_count = 1;
    float output[3] = {0};
    uint8_t nodata[3] = {0};
    pgaccel_status s = pgaccel_map_algebra(bands, 3, PGACCEL_PT_FLOAT32, &expr, output, nodata);
    ASSERT_STATUS_OK("LT status", s);
    ASSERT_NEAR("1 < 5 = 1", output[0], 1.0f, 0.01f);
    ASSERT_NEAR("5 < 5 = 0", output[1], 0.0f, 0.01f);
    ASSERT_NEAR("10 < 5 = 0", output[2], 0.0f, 0.01f);
  }
}

static void test_reclass_int32_to_int32() {
  printf("--- raster: reclass int32 to int32 ---\n");
  int32_t input[] = {0, 25, 50, 75, 100};
  pgaccel_reclass_rule rules[] = {
      {0, 50, 1},
      {50, 101, 2},
  };
  int32_t output[5] = {0};
  pgaccel_status s =
      pgaccel_raster_reclass(input, 5, PGACCEL_PT_INT32, rules, 2, PGACCEL_PT_INT32, output);
  ASSERT_STATUS_OK("int32 reclass status", s);
  ASSERT_EQ("reclass 0 -> 1", output[0], 1);
  ASSERT_EQ("reclass 25 -> 1", output[1], 1);
  ASSERT_EQ("reclass 50 -> 2", output[2], 2);
  ASSERT_EQ("reclass 75 -> 2", output[3], 2);
  ASSERT_EQ("reclass 100 -> 2", output[4], 2);
}

// =========================================================================
// main
// =========================================================================

int main() {
  printf("=== pgaccel Phase 8 correctness tests ===\n\n");

  pgaccel_status init_status = pgaccel_init();
  if (init_status != PGACCEL_OK) {
    fprintf(stderr, "pgaccel_init() failed with status %d\n", init_status);
    return 1;
  }

  // -- Spatial predicate edge cases --
  printf("\n== Spatial Predicates ==\n");
  test_point_in_ring_on_edge();
  test_point_in_ring_on_vertex();
  test_point_in_ring_degenerate_2pts();
  test_point_in_ring_identical_vertices();
  test_point_in_ring_large_circle();
  test_point_in_ring_bulk_0_points();
  test_point_in_ring_bulk_1_point();
  test_point_in_ring_concave_polygon();
  test_point_in_ring_fp32_near_edge();

  test_sphere_distance_identical();
  test_sphere_distance_antipodal();
  test_sphere_distance_near_antipodal();
  test_sphere_distance_poles();
  test_sphere_distance_bulk_0();
  test_sphere_distance_fp32_close();
  test_sphere_distance_batch_mixed();

  test_segment_intersects_collinear();
  test_segment_intersects_endpoint_touch();
  test_segment_intersects_parallel();
  test_segment_intersects_perpendicular();
  test_segment_intersects_bulk_0();
  test_segment_intersects_T_junction();
  test_segment_intersects_L_shape();
  test_segment_intersects_nearly_parallel();

  // -- Spatial dispatch edge cases --
  printf("\n== Spatial Dispatch ==\n");
  test_dispatch_0_geoms_a();
  test_dispatch_0_geoms_b();
  test_dispatch_point_equal();
  test_dispatch_point_not_equal();
  test_dispatch_line_crossing();
  test_dispatch_line_parallel();
  test_dispatch_polygon_with_hole_point_in_hole();
  test_dispatch_polygon_with_hole_point_in_shell();
  test_dispatch_unknown_geom_type();
  test_dispatch_null_bbox();

  // -- H3 edge cases --
  printf("\n== H3 ==\n");
  test_h3_resolution_0_to_15();
  test_h3_parent_same_res();
  test_h3_parent_res_0();
  test_h3_grid_distance_same_cell();
  test_h3_grid_distance_adjacent();
  test_h3_grid_distance_diff_resolution();
  test_h3_cell_to_parent_higher_res();
  test_h3_lat_lng_poles();
  test_h3_lat_lng_antimeridian();
  test_h3_lat_lng_origin();
  test_h3_lat_lng_bulk_0();
  test_h3_lat_lng_fp32_low_res();
  test_h3_lat_lng_fp32_high_res_exact_fixup();
  test_h3_get_resolution_invalid_cell();

  // -- Raster edge cases --
  printf("\n== Raster ==\n");
  test_map_algebra_empty_expr();
  test_map_algebra_div_zero();
  test_map_algebra_sqrt_negative();
  test_map_algebra_log_zero();
  test_map_algebra_0_pixels();
  test_map_algebra_abs();
  test_map_algebra_pow();
  test_map_algebra_comparison();
  test_raster_clip_0_vertices();
  test_raster_clip_triangle();
  test_raster_clip_0x0();
  test_raster_clip_1x1();
  test_reclass_0_rules();
  test_reclass_overlapping_rules();
  test_reclass_no_match_passthrough();
  test_reclass_int32_to_int32();

  // -- Bbox edge cases --
  printf("\n== Bbox ==\n");
  test_bbox_identical_boxes();
  test_bbox_touching_edge();
  test_bbox_touching_corner();
  test_bbox_zero_area_point();
  test_bbox_0_elements_a();
  test_bbox_0_elements_b();
  test_bbox_large_batch();
  test_bbox_separated();
  test_bbox_nested();

  // -- Hash join edge cases --
  printf("\n== Hash Join ==\n");
  test_hash_join_large_threshold_correctness();
  test_hash_join_small_build_is_supported();
  test_hash_join_max_matches_overflow_rejected();

  // -- fp64 round-trip sanity (W5 fp64-unlock plan) --
  // Round-trip a random fp64 vector through GPU reduce_sum_f64 and
  // compare to CPU sequential sum.
  //
  // Tolerance: tree-reduce-aware bound `max(8, log2(N) * 32)` ULP.
  // CPU oracle is sequential summation; GPU uses pairwise tree reduce.
  // Per Higham, "Accuracy and Stability of Numerical Algorithms" Theorem
  // 4.5, pairwise summation error grows linearly with reduction-tree
  // depth log2(N). Random U(-1000, 1000) data has |result| small
  // relative to sum_of_abs, so the 32x safety factor accommodates
  // partial cancellation. Same formula used for the matching budget
  // in test_reduce_stats.cpp.
  printf("\n== fp64 round-trip ==\n");
  {
    constexpr size_t N = 1048576;  // 1M — big enough to exercise tree reduce.
    std::mt19937_64 rng(0xDEADBEEFULL);
    std::uniform_real_distribution<double> dist(-1000.0, 1000.0);
    std::vector<double> v(N);
    for (size_t i = 0; i < N; ++i)
      v[i] = dist(rng);
    double cpu_sum = 0.0;
    for (size_t i = 0; i < N; ++i)
      cpu_sum += v[i];
    double gpu_sum = 0.0;
    pgaccel_status st = pgaccel_reduce_sum_f64(v.data(), N, &gpu_sum);
    ASSERT_EQ("fp64 round-trip status OK", st, PGACCEL_OK);
    uint64_t a, b;
    std::memcpy(&a, &gpu_sum, 8);
    std::memcpy(&b, &cpu_sum, 8);
    constexpr uint64_t SIGN = 0x8000000000000000ULL;
    a = (a & SIGN) ? ~a + 1 : a | SIGN;
    b = (b & SIGN) ? ~b + 1 : b | SIGN;
    uint64_t dist_ulp = a > b ? a - b : b - a;
    // Tree-reduce-aware budget: max(8, log2(N) * 32). For N=1M,
    // log2(N)=20, budget = max(8, 640) = 640 ULP.
    uint64_t log2_n = 0;
    for (size_t m = N; m > 1; m >>= 1)
      log2_n++;
    const uint64_t budget = std::max<uint64_t>(8, log2_n * 32);
    if (dist_ulp > budget) {
      fprintf(stderr, "  FAIL fp64 round-trip ULP: %llu > %llu  (gpu=%.17g cpu=%.17g)\n",
              (unsigned long long)dist_ulp, (unsigned long long)budget, gpu_sum, cpu_sum);
      g_tests_failed++;
    } else {
      g_tests_passed++;
    }
    g_tests_run++;
  }

  // -- fp64 sort (stable) --
  printf("\n== fp64 sort ==\n");
  for (size_t N : {(size_t)1024, (size_t)65536, (size_t)262144, (size_t)1048576}) {
    std::mt19937_64 rng(0xABCDEF ^ N);
    std::uniform_real_distribution<double> dist(-1e6, 1e6);
    std::vector<double> data(N);
    for (size_t i = 0; i < N; ++i)
      data[i] = dist(rng);
    std::vector<double> expected(data);
    std::sort(expected.begin(), expected.end());
    pgaccel_status st = pgaccel_sort_f64(data.data(), N);
    char buf[64];
    snprintf(buf, sizeof(buf), "sort_f64 N=%zu status OK", N);
    ASSERT_EQ(buf, st, PGACCEL_OK);
    // Compare bit-exact (0 ULP) — sort is a permutation, values aren't
    // mutated.
    bool bitmatch = true;
    for (size_t i = 0; i < N; ++i) {
      uint64_t got_bits, exp_bits;
      std::memcpy(&got_bits, &data[i], 8);
      std::memcpy(&exp_bits, &expected[i], 8);
      if (got_bits != exp_bits) {
        bitmatch = false;
        fprintf(stderr, "  FAIL sort_f64 N=%zu mismatch at i=%zu: got %.17g expected %.17g\n", N, i,
                data[i], expected[i]);
        break;
      }
    }
    snprintf(buf, sizeof(buf), "sort_f64 N=%zu bit-exact match", N);
    ASSERT_EQ(buf, bitmatch, true);
  }

  // -- fp64 kv-sort (stable) --
  printf("\n== fp64 kv-sort ==\n");
  for (size_t N : {(size_t)1024, (size_t)65536, (size_t)262144, (size_t)1048576}) {
    std::mt19937_64 rng(0xFEEDFACE ^ N);
    // Duplicate keys on purpose to exercise stability.
    std::uniform_int_distribution<int> key_int(0, 1000);
    std::vector<double> keys(N);
    std::vector<uint32_t> indices(N);
    for (size_t i = 0; i < N; ++i) {
      keys[i] = static_cast<double>(key_int(rng));
      indices[i] = static_cast<uint32_t>(i);
    }
    pgaccel_status st = pgaccel_sort_kv_f64(keys.data(), indices.data(), N);
    char buf[64];
    snprintf(buf, sizeof(buf), "sort_kv_f64 N=%zu status OK", N);
    ASSERT_EQ(buf, st, PGACCEL_OK);
    // Monotone keys check (bit-exact).
    bool monotone = true;
    for (size_t i = 1; i < N; ++i) {
      if (keys[i] < keys[i - 1]) {
        monotone = false;
        fprintf(stderr, "  FAIL sort_kv_f64 N=%zu non-monotone at i=%zu\n", N, i);
        break;
      }
    }
    snprintf(buf, sizeof(buf), "sort_kv_f64 N=%zu monotone keys", N);
    ASSERT_EQ(buf, monotone, true);
    // Stable-sort: among equal keys, original indices must be
    // non-decreasing.
    bool stable = true;
    for (size_t i = 1; i < N; ++i) {
      if (keys[i] == keys[i - 1] && indices[i] < indices[i - 1]) {
        stable = false;
        fprintf(stderr, "  FAIL sort_kv_f64 N=%zu stable-sort violated at i=%zu\n", N, i);
        break;
      }
    }
    snprintf(buf, sizeof(buf), "sort_kv_f64 N=%zu stable", N);
    ASSERT_EQ(buf, stable, true);
  }

  pgaccel_shutdown();

  printf("\n=== Results: %d/%d passed", g_tests_passed, g_tests_run);
  if (g_tests_failed > 0) {
    printf(", %d FAILED", g_tests_failed);
  }
  printf(" ===\n");

  return g_tests_failed > 0 ? 1 : 0;
}

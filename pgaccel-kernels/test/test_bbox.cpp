#include <cassert>
#include <cstdio>
#include <cstring>
#include <limits>
#include <vector>

#include "pgaccel_ffi.h"

static int tests_passed = 0;
static int tests_failed = 0;

#define CHECK(cond, msg)                                      \
  do {                                                        \
    if (!(cond)) {                                            \
      fprintf(stderr, "FAIL: %s (line %d)\n", msg, __LINE__); \
      ++tests_failed;                                         \
    } else {                                                  \
      ++tests_passed;                                         \
    }                                                         \
  } while (0)

static void test_empty_inputs() {
  size_t hits = 999;
  pgaccel_status s;

  s = pgaccel_bbox_intersects_bulk_f32(nullptr, 0, nullptr, 0, nullptr, &hits);
  CHECK(s == PGACCEL_OK, "empty f32 returns OK");
  CHECK(hits == 0, "empty f32 hits == 0");

  pgaccel_reset_gpu_exec_count();
  s = pgaccel_bbox_intersects_bulk_f64(nullptr, 0, nullptr, 0, nullptr, &hits);
  CHECK(s == PGACCEL_OK, "empty f64 returns OK");
  CHECK(hits == 0, "empty f64 hits == 0");
  CHECK(pgaccel_gpu_exec_count() == 0, "empty f64 does not record GPU execution");
}

static void test_single_pair_intersects() {
  // Two overlapping boxes: [0,0,2,2] and [1,1,3,3]
  float a[] = {0.0f, 0.0f, 2.0f, 2.0f};
  float b[] = {1.0f, 1.0f, 3.0f, 3.0f};
  uint8_t result = 0;
  size_t hits = 0;

  pgaccel_status s = pgaccel_bbox_intersects_bulk_f32(a, 1, b, 1, &result, &hits);
  CHECK(s == PGACCEL_OK, "single intersect returns OK");
  CHECK(result == 1, "overlapping boxes intersect");
  CHECK(hits == 1, "hit_count == 1");
}

static void test_single_pair_disjoint() {
  // Two non-overlapping boxes: [0,0,1,1] and [5,5,6,6]
  float a[] = {0.0f, 0.0f, 1.0f, 1.0f};
  float b[] = {5.0f, 5.0f, 6.0f, 6.0f};
  uint8_t result = 99;
  size_t hits = 99;

  pgaccel_status s = pgaccel_bbox_intersects_bulk_f32(a, 1, b, 1, &result, &hits);
  CHECK(s == PGACCEL_OK, "single disjoint returns OK");
  CHECK(result == 0, "disjoint boxes don't intersect");
  CHECK(hits == 0, "hit_count == 0");
}

static void test_edge_touching() {
  // Boxes share an edge: [0,0,1,1] and [1,0,2,1] — xmax == xmin
  float a[] = {0.0f, 0.0f, 1.0f, 1.0f};
  float b[] = {1.0f, 0.0f, 2.0f, 1.0f};
  uint8_t result = 0;
  size_t hits = 0;

  pgaccel_status s = pgaccel_bbox_intersects_bulk_f32(a, 1, b, 1, &result, &hits);
  CHECK(s == PGACCEL_OK, "edge-touching returns OK");
  CHECK(result == 1, "edge-touching boxes intersect");
  CHECK(hits == 1, "edge-touching hit_count == 1");
}

static void test_multi_pair() {
  // 2 x 3 = 6 pairs
  float a[] = {
      0.0f,  0.0f,  2.0f,  2.0f,   // A0: overlaps B0, B1, not B2
      10.0f, 10.0f, 12.0f, 12.0f,  // A1: overlaps B2, not B0, B1
  };
  float b[] = {
      1.0f,  1.0f,  3.0f,  3.0f,   // B0: overlaps A0
      -1.0f, -1.0f, 0.5f,  0.5f,   // B1: overlaps A0
      11.0f, 11.0f, 13.0f, 13.0f,  // B2: overlaps A1
  };
  uint8_t result[6] = {};
  size_t hits = 0;

  pgaccel_status s = pgaccel_bbox_intersects_bulk_f32(a, 2, b, 3, result, &hits);
  CHECK(s == PGACCEL_OK, "multi-pair returns OK");
  // result[i*3 + j]: A0xB0=1, A0xB1=1, A0xB2=0, A1xB0=0, A1xB1=0, A1xB2=1
  CHECK(result[0] == 1, "A0 x B0 intersects");
  CHECK(result[1] == 1, "A0 x B1 intersects");
  CHECK(result[2] == 0, "A0 x B2 disjoint");
  CHECK(result[3] == 0, "A1 x B0 disjoint");
  CHECK(result[4] == 0, "A1 x B1 disjoint");
  CHECK(result[5] == 1, "A1 x B2 intersects");
  CHECK(hits == 3, "multi-pair hit_count == 3");
}

static void test_f64_pair_semantics() {
  double a[] = {0.0, 0.0, 2.0, 2.0};
  double overlap[] = {1.0, 1.0, 3.0, 3.0};
  double disjoint[] = {5.0, 5.0, 6.0, 6.0};
  double touching[] = {2.0, -1.0, 4.0, 1.0};
  uint8_t result = 99;
  size_t hits = 99;

  pgaccel_reset_gpu_exec_count();
  pgaccel_status s = pgaccel_bbox_intersects_bulk_f64(a, 1, overlap, 1, &result, &hits);
  CHECK(s == PGACCEL_OK, "f64 overlap returns OK");
  CHECK(result == 1, "f64 overlapping boxes intersect");
  CHECK(hits == 1, "f64 overlap hit_count == 1");
  CHECK(pgaccel_gpu_exec_count() == 1, "f64 overlap records real GPU execution");

  s = pgaccel_bbox_intersects_bulk_f64(a, 1, disjoint, 1, &result, &hits);
  CHECK(s == PGACCEL_OK, "f64 disjoint returns OK");
  CHECK(result == 0, "f64 disjoint boxes do not intersect");
  CHECK(hits == 0, "f64 disjoint hit_count == 0");

  s = pgaccel_bbox_intersects_bulk_f64(a, 1, touching, 1, &result, &hits);
  CHECK(s == PGACCEL_OK, "f64 touching returns OK");
  CHECK(result == 1, "f64 touching boxes intersect");
  CHECK(hits == 1, "f64 touching hit_count == 1");
}

static void test_f64_nan_is_conservative() {
  const double nan = std::numeric_limits<double>::quiet_NaN();
  double uncertain_max[] = {0.0, 0.0, nan, 2.0};
  double distant[] = {5.0, 1.0, 6.0, 2.0};
  uint8_t result = 0;
  size_t hits = 0;

  pgaccel_status s = pgaccel_bbox_intersects_bulk_f64(uncertain_max, 1, distant, 1, &result, &hits);
  CHECK(s == PGACCEL_OK, "f64 NaN returns OK");
  CHECK(result == 1, "f64 NaN cannot create a bbox false negative");
  CHECK(hits == 1, "f64 NaN conservative pair counts as a hit");
}

static void test_f64_signed_zero_and_infinity() {
  const double infinity = std::numeric_limits<double>::infinity();
  double signed_zero_box[] = {-0.0, -0.0, +0.0, +0.0};
  double zero_touch[] = {+0.0, +0.0, 1.0, 1.0};
  double negative_infinite[] = {-infinity, -1.0, -1.0, 1.0};
  uint8_t result[2] = {};
  size_t hits = 0;

  pgaccel_status s =
      pgaccel_bbox_intersects_bulk_f64(signed_zero_box, 1, zero_touch, 1, &result[0], &hits);
  CHECK(s == PGACCEL_OK, "f64 signed-zero touching returns OK");
  CHECK(result[0] == 1 && hits == 1, "f64 signed zeros compare equal");

  s = pgaccel_bbox_intersects_bulk_f64(negative_infinite, 1, zero_touch, 1, &result[1], &hits);
  CHECK(s == PGACCEL_OK, "f64 infinity comparison returns OK");
  CHECK(result[1] == 0 && hits == 0, "f64 infinity preserves ordered disjointness");
}

static void test_optional_hits_empty_axes_and_negative_ordering() {
  float f32_box[] = {0.0f, 0.0f, 1.0f, 1.0f};
  double f64_box[] = {0.0, 0.0, 1.0, 1.0};
  uint8_t result = 0;

  CHECK(pgaccel_bbox_intersects_bulk_f32(f32_box, 1, f32_box, 1, &result, nullptr) == PGACCEL_OK,
        "f32 optional hit count returns OK");
  CHECK(result == 1, "f32 optional hit count preserves result");
  result = 0;
  CHECK(pgaccel_bbox_intersects_bulk_f64(f64_box, 1, f64_box, 1, &result, nullptr) == PGACCEL_OK,
        "f64 optional hit count returns OK");
  CHECK(result == 1, "f64 optional hit count preserves result");

  CHECK(pgaccel_bbox_intersects_bulk_f32(nullptr, 0, f32_box, 1, nullptr, nullptr) == PGACCEL_OK,
        "empty f32 left axis returns OK");
  CHECK(pgaccel_bbox_intersects_bulk_f32(f32_box, 1, nullptr, 0, nullptr, nullptr) == PGACCEL_OK,
        "empty f32 right axis returns OK");
  CHECK(pgaccel_bbox_intersects_bulk_f64(nullptr, 0, f64_box, 1, nullptr, nullptr) == PGACCEL_OK,
        "empty f64 left axis returns OK");
  CHECK(pgaccel_bbox_intersects_bulk_f64(f64_box, 1, nullptr, 0, nullptr, nullptr) == PGACCEL_OK,
        "empty f64 right axis returns OK");

  double negative_a[] = {-10.0, -1.0, -5.0, 1.0};
  double negative_b[] = {-4.0, -1.0, -3.0, 1.0};
  size_t hits = 99;
  result = 99;
  CHECK(pgaccel_bbox_intersects_bulk_f64(negative_a, 1, negative_b, 1, &result, &hits) ==
            PGACCEL_OK,
        "negative f64 ordering returns OK");
  CHECK(result == 0 && hits == 0, "negative f64 endpoints preserve ordered disjointness");
}

static void test_f64_multi_pair_layout() {
  double a[] = {
      0.0, 0.0, 2.0, 2.0, 10.0, 10.0, 12.0, 12.0,
  };
  double b[] = {
      1.0, 1.0, 3.0, 3.0, -1.0, -1.0, 0.5, 0.5, 11.0, 11.0, 13.0, 13.0,
  };
  uint8_t result[6] = {};
  size_t hits = 0;

  pgaccel_status s = pgaccel_bbox_intersects_bulk_f64(a, 2, b, 3, result, &hits);
  CHECK(s == PGACCEL_OK, "f64 multi-pair returns OK");
  const uint8_t expected[] = {1, 1, 0, 0, 0, 1};
  CHECK(std::memcmp(result, expected, sizeof(expected)) == 0,
        "f64 multi-pair result uses row-major pair layout");
  CHECK(hits == 3, "f64 multi-pair hit_count == 3");
}

static void test_f64_invalid_and_overflow() {
  double box[] = {0.0, 0.0, 1.0, 1.0};
  uint8_t result = 0;
  size_t hits = 0;

  pgaccel_reset_gpu_exec_count();
  CHECK(pgaccel_bbox_intersects_bulk_f64(nullptr, 1, box, 1, &result, &hits) == PGACCEL_ERROR,
        "null f64 boxes_a returns ERROR");
  CHECK(pgaccel_bbox_intersects_bulk_f64(box, 1, nullptr, 1, &result, &hits) == PGACCEL_ERROR,
        "null f64 boxes_b returns ERROR");
  CHECK(pgaccel_bbox_intersects_bulk_f64(box, 1, box, 1, nullptr, &hits) == PGACCEL_ERROR,
        "null f64 result returns ERROR");

  const size_t overflowing_box_count = std::numeric_limits<size_t>::max() / 4 + 1;
  CHECK(pgaccel_bbox_intersects_bulk_f64(box, overflowing_box_count, box, 1, &result, &hits) ==
            PGACCEL_ERROR,
        "overflowing f64 box count returns ERROR");
  CHECK(pgaccel_bbox_intersects_bulk_f64(box, 1, box, overflowing_box_count, &result, &hits) ==
            PGACCEL_ERROR,
        "overflowing f64 right box count returns ERROR");
  const size_t overflowing_byte_count = std::numeric_limits<size_t>::max() / sizeof(uint64_t) + 1;
  CHECK(pgaccel_bbox_intersects_bulk_f64(box, overflowing_byte_count, box, 1, &result, &hits) ==
            PGACCEL_ERROR,
        "overflowing f64 byte count returns ERROR");
  const size_t pair_overflow_count = std::numeric_limits<size_t>::max() / 32;
  CHECK(pgaccel_bbox_intersects_bulk_f64(box, pair_overflow_count, box, 33, &result, &hits) ==
            PGACCEL_ERROR,
        "overflowing f64 pair product returns ERROR");
  CHECK(pgaccel_gpu_exec_count() == 0, "invalid f64 calls do not record GPU execution");
}

static void test_null_pointers() {
  float a[] = {0.0f, 0.0f, 1.0f, 1.0f};
  uint8_t result = 0;
  size_t hits = 0;

  pgaccel_status s = pgaccel_bbox_intersects_bulk_f32(nullptr, 1, a, 1, &result, &hits);
  CHECK(s == PGACCEL_ERROR, "null boxes_a returns ERROR");

  s = pgaccel_bbox_intersects_bulk_f32(a, 1, nullptr, 1, &result, &hits);
  CHECK(s == PGACCEL_ERROR, "null boxes_b returns ERROR");

  s = pgaccel_bbox_intersects_bulk_f32(a, 1, a, 1, nullptr, &hits);
  CHECK(s == PGACCEL_ERROR, "null result returns ERROR");

  const size_t overflowing_box_count = std::numeric_limits<size_t>::max() / 4 + 1;
  s = pgaccel_bbox_intersects_bulk_f32(a, overflowing_box_count, a, 1, &result, &hits);
  CHECK(s == PGACCEL_ERROR, "overflowing f32 box count returns ERROR");
  s = pgaccel_bbox_intersects_bulk_f32(a, 1, a, overflowing_box_count, &result, &hits);
  CHECK(s == PGACCEL_ERROR, "overflowing f32 right box count returns ERROR");
  s = pgaccel_bbox_intersects_bulk_f32(a, std::numeric_limits<size_t>::max() / 4, a, 5,
                                       &result, &hits);
  CHECK(s == PGACCEL_ERROR, "overflowing f32 pair product returns ERROR");
}

int main() {
  pgaccel_init();

  printf("Running bbox overlap tests...\n");
  test_empty_inputs();
  test_single_pair_intersects();
  test_single_pair_disjoint();
  test_edge_touching();
  test_multi_pair();
  test_f64_pair_semantics();
  test_f64_nan_is_conservative();
  test_f64_signed_zero_and_infinity();
  test_optional_hits_empty_axes_and_negative_ordering();
  test_f64_multi_pair_layout();
  test_f64_invalid_and_overflow();
  test_null_pointers();

  printf("\nResults: %d passed, %d failed\n", tests_passed, tests_failed);

  pgaccel_shutdown();
  return tests_failed > 0 ? 1 : 0;
}

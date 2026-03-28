#include "pgaccel_ffi.h"
#include <cassert>
#include <cstdio>
#include <cstring>
#include <vector>

static int tests_passed = 0;
static int tests_failed = 0;

#define CHECK(cond, msg)                                            \
    do {                                                            \
        if (!(cond)) {                                              \
            fprintf(stderr, "FAIL: %s (line %d)\n", msg, __LINE__);\
            ++tests_failed;                                         \
        } else {                                                    \
            ++tests_passed;                                         \
        }                                                           \
    } while (0)

static void test_empty_inputs() {
    size_t hits = 999;
    pgaccel_status s;

    s = pgaccel_bbox_intersects_bulk_f32(nullptr, 0, nullptr, 0, nullptr, &hits);
    CHECK(s == PGACCEL_OK, "empty f32 returns OK");
    CHECK(hits == 0, "empty f32 hits == 0");

    s = pgaccel_bbox_intersects_bulk_f64(nullptr, 0, nullptr, 0, nullptr, &hits);
    CHECK(s == PGACCEL_OK, "empty f64 returns OK");
    CHECK(hits == 0, "empty f64 hits == 0");
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
        0.0f, 0.0f, 2.0f, 2.0f,  // A0: overlaps B0, B1, not B2
        10.0f, 10.0f, 12.0f, 12.0f,  // A1: overlaps B2, not B0, B1
    };
    float b[] = {
        1.0f, 1.0f, 3.0f, 3.0f,    // B0: overlaps A0
        -1.0f, -1.0f, 0.5f, 0.5f,  // B1: overlaps A0
        11.0f, 11.0f, 13.0f, 13.0f, // B2: overlaps A1
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

static void test_f64_basic() {
    double a[] = {0.0, 0.0, 2.0, 2.0};
    double b[] = {1.0, 1.0, 3.0, 3.0};
    uint8_t result = 0;
    size_t hits = 0;

    pgaccel_status s = pgaccel_bbox_intersects_bulk_f64(a, 1, b, 1, &result, &hits);
    // On CPU fallback, fp64 is supported
    if (s == PGACCEL_UNSUPPORTED) {
        printf("  (fp64 unsupported on this platform, skipping)\n");
        return;
    }
    CHECK(s == PGACCEL_OK, "f64 basic returns OK");
    CHECK(result == 1, "f64 overlapping boxes intersect");
    CHECK(hits == 1, "f64 hit_count == 1");
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
}

int main() {
    pgaccel_init();

    printf("Running bbox overlap tests...\n");
    test_empty_inputs();
    test_single_pair_intersects();
    test_single_pair_disjoint();
    test_edge_touching();
    test_multi_pair();
    test_f64_basic();
    test_null_pointers();

    printf("\nResults: %d passed, %d failed\n", tests_passed, tests_failed);

    pgaccel_shutdown();
    return tests_failed > 0 ? 1 : 0;
}

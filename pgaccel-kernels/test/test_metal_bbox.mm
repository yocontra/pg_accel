// test_metal_bbox — Tests the native Metal backend bbox intersection kernel.

#include "metal_backend.h"
#include <cstdio>
#include <cstdlib>
#include <cstring>
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

static void test_basic_intersection() {
    // Box A: [0,0,2,2], Box B: [1,1,3,3] → intersect
    float boxes_a[] = { 0.0f, 0.0f, 2.0f, 2.0f };
    float boxes_b[] = { 1.0f, 1.0f, 3.0f, 3.0f };
    uint8_t result[1] = {};
    size_t hits = 0;

    metal_status st = metal_bbox_intersects_f32(boxes_a, 1, boxes_b, 1, result, &hits);
    CHECK(st == METAL_OK, "basic intersect status=%d", st);
    CHECK(result[0] == 1, "basic intersect result=%d (expected 1)", result[0]);
    CHECK(hits == 1, "basic intersect hits=%zu (expected 1)", hits);
}

static void test_no_intersection() {
    // Box A: [0,0,1,1], Box B: [2,2,3,3] → no intersect
    float boxes_a[] = { 0.0f, 0.0f, 1.0f, 1.0f };
    float boxes_b[] = { 2.0f, 2.0f, 3.0f, 3.0f };
    uint8_t result[1] = {};
    size_t hits = 0;

    metal_status st = metal_bbox_intersects_f32(boxes_a, 1, boxes_b, 1, result, &hits);
    CHECK(st == METAL_OK, "no intersect status=%d", st);
    CHECK(result[0] == 0, "no intersect result=%d (expected 0)", result[0]);
    CHECK(hits == 0, "no intersect hits=%zu (expected 0)", hits);
}

static void test_cross_product() {
    // 3 A-boxes × 2 B-boxes = 6 pairs
    float boxes_a[] = {
        0.0f, 0.0f, 1.0f, 1.0f,   // A0
        2.0f, 2.0f, 4.0f, 4.0f,   // A1
        5.0f, 5.0f, 6.0f, 6.0f,   // A2
    };
    float boxes_b[] = {
        0.5f, 0.5f, 3.0f, 3.0f,   // B0: overlaps A0, A1
        10.0f, 10.0f, 11.0f, 11.0f, // B1: overlaps nothing
    };
    uint8_t result[6] = {};
    size_t hits = 0;

    metal_status st = metal_bbox_intersects_f32(boxes_a, 3, boxes_b, 2, result, &hits);
    CHECK(st == METAL_OK, "cross product status=%d", st);

    // A0×B0=1, A0×B1=0, A1×B0=1, A1×B1=0, A2×B0=0, A2×B1=0
    CHECK(result[0] == 1, "A0×B0=%d (expected 1)", result[0]);
    CHECK(result[1] == 0, "A0×B1=%d (expected 0)", result[1]);
    CHECK(result[2] == 1, "A1×B0=%d (expected 1)", result[2]);
    CHECK(result[3] == 0, "A1×B1=%d (expected 0)", result[3]);
    CHECK(result[4] == 0, "A2×B0=%d (expected 0)", result[4]);
    CHECK(result[5] == 0, "A2×B1=%d (expected 0)", result[5]);
    CHECK(hits == 2, "cross product hits=%zu (expected 2)", hits);
}

static void test_touching_boxes() {
    // Touching at edge: A=[0,0,1,1], B=[1,0,2,1] → xmax==xmin → NOT intersect
    float boxes_a[] = { 0.0f, 0.0f, 1.0f, 1.0f };
    float boxes_b[] = { 1.0f, 0.0f, 2.0f, 1.0f };
    uint8_t result[1] = {};
    size_t hits = 0;

    metal_status st = metal_bbox_intersects_f32(boxes_a, 1, boxes_b, 1, result, &hits);
    CHECK(st == METAL_OK, "touching status=%d", st);
    // a_xmax (1.0) < b_xmin (1.0) is false, a_xmin (0.0) > b_xmax (2.0) is false
    // a_ymax (1.0) < b_ymin (0.0) is false, a_ymin (0.0) > b_ymax (1.0) is false
    // NOT all disjoint → intersects=true (touching counts as intersect)
    CHECK(result[0] == 1, "touching result=%d (expected 1, touching=intersect)", result[0]);
}

static void test_large_batch() {
    const size_t N = 1000;
    std::vector<float> boxes_a(N * 4), boxes_b(N * 4);

    // Create N boxes along a line, each 1×1
    for (size_t i = 0; i < N; i++) {
        float x = (float)i * 0.5f; // 50% overlap with neighbors
        boxes_a[i * 4 + 0] = x;
        boxes_a[i * 4 + 1] = 0.0f;
        boxes_a[i * 4 + 2] = x + 1.0f;
        boxes_a[i * 4 + 3] = 1.0f;

        boxes_b[i * 4 + 0] = x;
        boxes_b[i * 4 + 1] = 0.0f;
        boxes_b[i * 4 + 2] = x + 1.0f;
        boxes_b[i * 4 + 3] = 1.0f;
    }

    std::vector<uint8_t> result(N * N);
    size_t hits = 0;

    metal_status st = metal_bbox_intersects_f32(
        boxes_a.data(), N, boxes_b.data(), N, result.data(), &hits);
    CHECK(st == METAL_OK, "large batch 1000×1000 status=%d", st);
    CHECK(hits > 0, "large batch hits=%zu (expected > 0)", hits);

    // Diagonal should all be 1 (same box intersects itself)
    bool diag_ok = true;
    for (size_t i = 0; i < N; i++) {
        if (result[i * N + i] != 1) { diag_ok = false; break; }
    }
    CHECK(diag_ok, "large batch: diagonal all intersect");
}

int main() {
    printf("=== Metal Backend BBox Tests ===\n\n");

    setenv("PGACCEL_METALLIB_PATH", "pgaccel_kernels.metallib", 1);

    metal_status st = metal_init();
    if (st != METAL_OK) {
        fprintf(stderr, "metal_init failed: %d\n", st);
        return 1;
    }

    metal_device_info info = metal_get_device_info();
    printf("Device: %s (unified=%d)\n\n", info.device_name, info.is_unified_memory);

    test_basic_intersection();
    test_no_intersection();
    test_cross_product();
    test_touching_boxes();
    test_large_batch();

    printf("\n=== Results: %d failures ===\n", failures);
    metal_shutdown();
    return failures > 0 ? 1 : 0;
}

// test_metal_fused — Tests the native Metal backend fused filter+reduce kernels.

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

#define NEAR(a, b, tol) (fabs((double)(a) - (double)(b)) < (tol))

static void test_fused_filter_sum() {
    // filter: values > 5.0, agg: SUM
    float filter[] = { 1.0f, 6.0f, 3.0f, 8.0f, 2.0f, 10.0f };
    float agg[]    = { 10.0f, 20.0f, 30.0f, 40.0f, 50.0f, 60.0f };
    double result = -1.0;

    // cmp_op=4 (GT), agg_op=0 (SUM)
    metal_status st = metal_fused_filter_reduce_f32(
        filter, 4, 5.0f, agg, 0, 6, &result);
    CHECK(st == METAL_OK, "fused sum status=%d", st);
    // filter[1]=6>5, filter[3]=8>5, filter[5]=10>5 → sum agg[1]+agg[3]+agg[5] = 20+40+60 = 120
    CHECK(NEAR(result, 120.0, 1.0), "fused sum result=%.1f (expected 120.0)", result);
}

static void test_fused_filter_min() {
    float filter[] = { 1.0f, 6.0f, 3.0f, 8.0f, 2.0f, 10.0f };
    float agg[]    = { 10.0f, 20.0f, 30.0f, 40.0f, 50.0f, 60.0f };
    double result = -1.0;

    // cmp_op=4 (GT), agg_op=1 (MIN)
    metal_status st = metal_fused_filter_reduce_f32(
        filter, 4, 5.0f, agg, 1, 6, &result);
    CHECK(st == METAL_OK, "fused min status=%d", st);
    // Matching: agg[1]=20, agg[3]=40, agg[5]=60 → min=20
    CHECK(NEAR(result, 20.0, 1.0), "fused min result=%.1f (expected 20.0)", result);
}

static void test_fused_filter_max() {
    float filter[] = { 1.0f, 6.0f, 3.0f, 8.0f, 2.0f, 10.0f };
    float agg[]    = { 10.0f, 20.0f, 30.0f, 40.0f, 50.0f, 60.0f };
    double result = -1.0;

    // cmp_op=4 (GT), agg_op=2 (MAX)
    metal_status st = metal_fused_filter_reduce_f32(
        filter, 4, 5.0f, agg, 2, 6, &result);
    CHECK(st == METAL_OK, "fused max status=%d", st);
    CHECK(NEAR(result, 60.0, 1.0), "fused max result=%.1f (expected 60.0)", result);
}

static void test_fused_filter_count() {
    float filter[] = { 1.0f, 6.0f, 3.0f, 8.0f, 2.0f, 10.0f };
    int64_t count = -1;

    // cmp_op=4 (GT), filter_val=5.0 → 3 matches
    metal_status st = metal_fused_filter_count_f32(filter, 4, 5.0f, 6, &count);
    CHECK(st == METAL_OK, "fused count status=%d", st);
    CHECK(count == 3, "fused count=%lld (expected 3)", (long long)count);
}

static void test_fused_no_matches() {
    float filter[] = { 1.0f, 2.0f, 3.0f };
    float agg[]    = { 10.0f, 20.0f, 30.0f };
    double result = -1.0;

    // cmp_op=4 (GT), filter_val=100.0 → 0 matches
    metal_status st = metal_fused_filter_reduce_f32(
        filter, 4, 100.0f, agg, 0, 3, &result);
    CHECK(st == METAL_OK, "fused no_matches status=%d", st);
    CHECK(NEAR(result, 0.0, 0.01), "fused no_matches sum=%.1f (expected 0.0)", result);
}

static void test_fused_eq_operator() {
    float filter[] = { 1.0f, 2.0f, 3.0f, 2.0f, 1.0f };
    float agg[]    = { 10.0f, 20.0f, 30.0f, 40.0f, 50.0f };
    double result = -1.0;

    // cmp_op=0 (EQ), filter_val=2.0 → matches idx 1,3
    metal_status st = metal_fused_filter_reduce_f32(
        filter, 0, 2.0f, agg, 0, 5, &result);
    CHECK(st == METAL_OK, "fused EQ status=%d", st);
    CHECK(NEAR(result, 60.0, 1.0), "fused EQ sum=%.1f (expected 60.0=20+40)", result);
}

static void test_fused_large_batch() {
    const size_t N = 100000;
    std::vector<float> filter(N), agg(N);

    // Even indices > 5.0, odd indices <= 5.0
    for (size_t i = 0; i < N; i++) {
        filter[i] = (i % 2 == 0) ? 10.0f : 1.0f;
        agg[i] = 1.0f;
    }

    double result = -1.0;
    // cmp_op=4 (GT), filter_val=5.0, agg_op=0 (SUM)
    metal_status st = metal_fused_filter_reduce_f32(
        filter.data(), 4, 5.0f, agg.data(), 0, N, &result);
    CHECK(st == METAL_OK, "fused large status=%d", st);
    // 50000 even indices match, each agg=1.0 → sum=50000
    CHECK(NEAR(result, 50000.0, 10.0),
          "fused large sum=%.0f (expected ~50000)", result);
}

static void test_fused_count_large() {
    const size_t N = 100000;
    std::vector<float> filter(N);
    for (size_t i = 0; i < N; i++) {
        filter[i] = (float)(i % 10);
    }

    int64_t count = -1;
    // cmp_op=2 (LT), filter_val=3.0 → values 0,1,2 match = 30000
    metal_status st = metal_fused_filter_count_f32(
        filter.data(), 2, 3.0f, N, &count);
    CHECK(st == METAL_OK, "fused count large status=%d", st);
    CHECK(count == 30000, "fused count large=%lld (expected 30000)", (long long)count);
}

int main() {
    printf("=== Metal Backend Fused Tests ===\n\n");

    setenv("PGACCEL_METALLIB_PATH", "pgaccel_kernels.metallib", 1);

    metal_status st = metal_init();
    if (st != METAL_OK) {
        fprintf(stderr, "metal_init failed: %d\n", st);
        return 1;
    }

    metal_device_info info = metal_get_device_info();
    printf("Device: %s (unified=%d)\n\n", info.device_name, info.is_unified_memory);

    test_fused_filter_sum();
    test_fused_filter_min();
    test_fused_filter_max();
    test_fused_filter_count();
    test_fused_no_matches();
    test_fused_eq_operator();
    test_fused_large_batch();
    test_fused_count_large();

    printf("\n=== Results: %d failures ===\n", failures);
    metal_shutdown();
    return failures > 0 ? 1 : 0;
}

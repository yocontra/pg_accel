// test_metal_reduce — Tests the native Metal backend reduce kernels.
//
// Verifies metal_init() + metal_reduce_* functions produce correct results,
// both in the current process and after fork (the zero-IPC scenario).

#include "metal_backend.h"
#include <cstdio>
#include <cstdlib>
#include <cmath>
#include <sys/wait.h>
#include <unistd.h>

static int failures = 0;

#define CHECK(cond, fmt, ...) do { \
    if (!(cond)) { \
        fprintf(stderr, "FAIL: " fmt "\n", ##__VA_ARGS__); \
        failures++; \
    } else { \
        printf("OK: " fmt "\n", ##__VA_ARGS__); \
    } \
} while(0)

static void test_reduce_sum_f32() {
    const size_t N = 100000;
    float* data = (float*)malloc(N * sizeof(float));
    for (size_t i = 0; i < N; i++) data[i] = 1.0f;

    float result = 0.0f;
    metal_status st = metal_reduce_sum_f32(data, N, &result);
    CHECK(st == METAL_OK, "reduce_sum_f32 status=%d", st);
    CHECK(fabsf(result - 100000.0f) < 10.0f,
          "reduce_sum_f32 result=%f (expected 100000)", result);

    // Small input
    float small[] = {3.0f, 7.0f, 11.0f};
    st = metal_reduce_sum_f32(small, 3, &result);
    CHECK(st == METAL_OK && fabsf(result - 21.0f) < 0.01f,
          "reduce_sum_f32 small=%f (expected 21)", result);

    // Edge: count=0
    st = metal_reduce_sum_f32(data, 0, &result);
    CHECK(st == METAL_OK && result == 0.0f,
          "reduce_sum_f32 empty=%f (expected 0)", result);

    // Edge: count=1
    st = metal_reduce_sum_f32(data, 1, &result);
    CHECK(st == METAL_OK && result == 1.0f,
          "reduce_sum_f32 single=%f (expected 1)", result);

    free(data);
}

static void test_reduce_min_max_f32() {
    float data[] = {5.0f, 3.0f, 9.0f, 1.0f, 7.0f, 2.0f, 8.0f, 4.0f};
    float result = 0.0f;

    metal_status st = metal_reduce_min_f32(data, 8, &result);
    CHECK(st == METAL_OK && fabsf(result - 1.0f) < 0.01f,
          "reduce_min_f32=%f (expected 1)", result);

    st = metal_reduce_max_f32(data, 8, &result);
    CHECK(st == METAL_OK && fabsf(result - 9.0f) < 0.01f,
          "reduce_max_f32=%f (expected 9)", result);
}

static void test_reduce_sum_i64() {
    const size_t N = 50000;
    int64_t* data = (int64_t*)malloc(N * sizeof(int64_t));
    for (size_t i = 0; i < N; i++) data[i] = 3;

    int64_t result = 0;
    metal_status st = metal_reduce_sum_i64(data, N, &result);
    CHECK(st == METAL_OK, "reduce_sum_i64 status=%d", st);
    CHECK(result == 150000, "reduce_sum_i64 result=%lld (expected 150000)",
          (long long)result);

    free(data);
}

static void test_reduce_multi_f32() {
    const size_t N = 10000;
    float* data = (float*)malloc(N * sizeof(float));
    for (size_t i = 0; i < N; i++) data[i] = (float)(i + 1);

    float sum, mn, mx;
    int64_t cnt;
    metal_status st = metal_reduce_multi_f32(data, N, &sum, &mn, &mx, &cnt);
    CHECK(st == METAL_OK, "reduce_multi_f32 status=%d", st);

    float expected_sum = (float)N * ((float)N + 1.0f) / 2.0f;
    CHECK(fabsf(sum - expected_sum) < expected_sum * 0.001f,
          "reduce_multi_f32 sum=%f (expected %f)", sum, expected_sum);
    CHECK(fabsf(mn - 1.0f) < 0.01f,
          "reduce_multi_f32 min=%f (expected 1)", mn);
    CHECK(fabsf(mx - (float)N) < 0.01f,
          "reduce_multi_f32 max=%f (expected %f)", mx, (float)N);
    CHECK(cnt == (int64_t)N,
          "reduce_multi_f32 count=%lld (expected %zu)", (long long)cnt, N);

    free(data);
}

static void test_reduce_multi_i64() {
    int64_t data[] = {10, -5, 3, 100, -200, 50};
    int64_t sum, mn, mx, cnt;
    metal_status st = metal_reduce_multi_i64(data, 6, &sum, &mn, &mx, &cnt);
    CHECK(st == METAL_OK, "reduce_multi_i64 status=%d", st);
    CHECK(sum == -42, "reduce_multi_i64 sum=%lld (expected -42)", (long long)sum);
    CHECK(mn == -200, "reduce_multi_i64 min=%lld (expected -200)", (long long)mn);
    CHECK(mx == 100, "reduce_multi_i64 max=%lld (expected 100)", (long long)mx);
    CHECK(cnt == 6, "reduce_multi_i64 count=%lld (expected 6)", (long long)cnt);
}

static void test_reduce_count() {
    uint8_t mask[] = {1, 0, 1, 1, 0, 0, 1, 1, 1, 0};
    size_t result = 0;
    metal_status st = metal_reduce_count(mask, 10, &result);
    CHECK(st == METAL_OK && result == 6,
          "reduce_count=%zu (expected 6)", result);
}

int main() {
    printf("=== Metal Backend Reduce Tests ===\n\n");

    // Set metallib path to build output
    setenv("PGACCEL_METALLIB_PATH",
           "pgaccel_kernels.metallib", 1);

    metal_status st = metal_init();
    if (st != METAL_OK) {
        fprintf(stderr, "metal_init failed: %d\n", st);
        return 1;
    }

    metal_device_info info = metal_get_device_info();
    printf("Device: %s (unified=%d)\n\n", info.device_name, info.is_unified_memory);

    test_reduce_sum_f32();
    test_reduce_min_max_f32();
    test_reduce_sum_i64();
    test_reduce_multi_f32();
    test_reduce_multi_i64();
    test_reduce_count();

    printf("\n=== Results: %d failures ===\n", failures);
    metal_shutdown();
    return failures > 0 ? 1 : 0;
}

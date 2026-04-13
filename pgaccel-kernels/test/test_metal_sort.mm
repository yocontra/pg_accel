// test_metal_sort — Tests the native Metal backend sort kernels.
//
// Verifies metal_sort_kv_u32/u64 produce correct sorted output for:
//   - Small arrays (bitonic path)
//   - Large arrays (radix path, 100K+ elements)
//   - Stability (equal keys preserve original index order)
//   - Signed integer and float encodings

#include "metal_backend.h"
#include <cstdio>
#include <cstdlib>
#include <cstring>
#include <cmath>
#include <algorithm>
#include <numeric>
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

// Sortable-uint conversions (same as sort.cpp)
static inline uint32_t i32_to_sortable(int32_t v) {
    return (uint32_t)v ^ 0x80000000u;
}
static inline int32_t sortable_to_i32(uint32_t u) {
    return (int32_t)(u ^ 0x80000000u);
}
static inline uint32_t f32_to_sortable(float f) {
    if (f != f) f = __builtin_nanf("");
    uint32_t bits;
    memcpy(&bits, &f, sizeof(bits));
    uint32_t mask = (bits & 0x80000000u) ? 0xFFFFFFFFu : 0x80000000u;
    return bits ^ mask;
}
static inline float sortable_to_f32(uint32_t u) {
    uint32_t mask = (u & 0x80000000u) ? 0x80000000u : 0xFFFFFFFFu;
    uint32_t bits = u ^ mask;
    float f;
    memcpy(&f, &bits, sizeof(f));
    return f;
}
static inline uint64_t i64_to_sortable(int64_t v) {
    return (uint64_t)v ^ 0x8000000000000000ULL;
}
static inline int64_t sortable_to_i64(uint64_t u) {
    return (int64_t)(u ^ 0x8000000000000000ULL);
}

// ── Tests ─────────────────────────────────────────────────────────

static void test_sort_small_u32() {
    uint32_t keys[] = {5, 3, 8, 1, 9, 2, 7, 4, 6, 0};
    uint32_t indices[] = {0, 1, 2, 3, 4, 5, 6, 7, 8, 9};
    metal_status st = metal_sort_kv_u32(keys, indices, 10);
    CHECK(st == METAL_OK, "sort_kv_u32 small status=%d", st);
    bool sorted = true;
    for (int i = 1; i < 10; i++) {
        if (keys[i] < keys[i-1]) { sorted = false; break; }
    }
    CHECK(sorted, "sort_kv_u32 small: sorted correctly");
    CHECK(keys[0] == 0 && keys[9] == 9, "sort_kv_u32 small: min=%u max=%u",
          keys[0], keys[9]);
}

static void test_sort_large_u32() {
    const size_t N = 100000;
    std::vector<uint32_t> keys(N);
    std::vector<uint32_t> indices(N);
    // Reverse order
    for (size_t i = 0; i < N; i++) {
        keys[i] = (uint32_t)(N - 1 - i);
        indices[i] = (uint32_t)i;
    }

    metal_status st = metal_sort_kv_u32(keys.data(), indices.data(), N);
    CHECK(st == METAL_OK, "sort_kv_u32 100K status=%d", st);
    bool sorted = true;
    for (size_t i = 1; i < N; i++) {
        if (keys[i] < keys[i-1]) { sorted = false; break; }
    }
    CHECK(sorted, "sort_kv_u32 100K: sorted correctly");
    CHECK(keys[0] == 0 && keys[N-1] == (uint32_t)(N-1),
          "sort_kv_u32 100K: min=%u max=%u", keys[0], keys[N-1]);
}

static void test_sort_stability_u32() {
    // 10 elements, all keys=42, indices 0..9
    // After stable sort, indices should remain 0..9
    const size_t N = 10;
    std::vector<uint32_t> keys(N, 42);
    std::vector<uint32_t> indices(N);
    std::iota(indices.begin(), indices.end(), 0);

    metal_status st = metal_sort_kv_u32(keys.data(), indices.data(), N);
    CHECK(st == METAL_OK, "sort stability status=%d", st);
    bool stable = true;
    for (size_t i = 0; i < N; i++) {
        if (indices[i] != (uint32_t)i) { stable = false; break; }
    }
    CHECK(stable, "sort_kv_u32: stable for equal keys");
}

static void test_sort_i32_via_u32() {
    // Signed integers: convert to sortable u32, sort, convert back
    int32_t data[] = {5, -3, 8, -1, 0, -100, 42, -42};
    const size_t N = 8;
    std::vector<uint32_t> ukeys(N);
    std::vector<uint32_t> indices(N);
    for (size_t i = 0; i < N; i++) {
        ukeys[i] = i32_to_sortable(data[i]);
        indices[i] = (uint32_t)i;
    }

    metal_status st = metal_sort_kv_u32(ukeys.data(), indices.data(), N);
    CHECK(st == METAL_OK, "sort i32 via u32 status=%d", st);

    int32_t sorted[N];
    for (size_t i = 0; i < N; i++) sorted[i] = sortable_to_i32(ukeys[i]);

    CHECK(sorted[0] == -100 && sorted[1] == -42 && sorted[N-1] == 42,
          "sort i32: [-100, -42, ..., 42] got [%d, %d, ..., %d]",
          sorted[0], sorted[1], sorted[N-1]);
}

static void test_sort_f32_via_u32() {
    float data[] = {3.14f, -2.0f, 0.0f, -0.0f, 1e10f, -1e10f, NAN};
    const size_t N = 7;
    std::vector<uint32_t> ukeys(N);
    std::vector<uint32_t> indices(N);
    for (size_t i = 0; i < N; i++) {
        ukeys[i] = f32_to_sortable(data[i]);
        indices[i] = (uint32_t)i;
    }

    metal_status st = metal_sort_kv_u32(ukeys.data(), indices.data(), N);
    CHECK(st == METAL_OK, "sort f32 via u32 status=%d", st);

    float sorted[N];
    for (size_t i = 0; i < N; i++) sorted[i] = sortable_to_f32(ukeys[i]);

    // Expected: -1e10, -2.0, -0.0, 0.0, 3.14, 1e10, NaN
    CHECK(sorted[0] == -1e10f, "sort f32: min=%f (expected -1e10)", sorted[0]);
    CHECK(sorted[N-2] == 1e10f, "sort f32: second-last=%f (expected 1e10)", sorted[N-2]);
    CHECK(std::isnan(sorted[N-1]), "sort f32: NaN sorts last");
}

static void test_sort_large_u64() {
    const size_t N = 100000;
    std::vector<uint64_t> keys(N);
    std::vector<uint32_t> indices(N);
    for (size_t i = 0; i < N; i++) {
        keys[i] = (uint64_t)(N - 1 - i);
        indices[i] = (uint32_t)i;
    }

    metal_status st = metal_sort_kv_u64(keys.data(), indices.data(), N);
    CHECK(st == METAL_OK, "sort_kv_u64 100K status=%d", st);
    bool sorted = true;
    for (size_t i = 1; i < N; i++) {
        if (keys[i] < keys[i-1]) { sorted = false; break; }
    }
    CHECK(sorted, "sort_kv_u64 100K: sorted correctly");
}

static void test_sort_i64_via_u64() {
    int64_t data[] = {100, -50, 0, INT64_MAX, INT64_MIN, 42, -42};
    const size_t N = 7;
    std::vector<uint64_t> ukeys(N);
    std::vector<uint32_t> indices(N);
    for (size_t i = 0; i < N; i++) {
        ukeys[i] = i64_to_sortable(data[i]);
        indices[i] = (uint32_t)i;
    }

    metal_status st = metal_sort_kv_u64(ukeys.data(), indices.data(), N);
    CHECK(st == METAL_OK, "sort i64 via u64 status=%d", st);

    int64_t sorted[N];
    for (size_t i = 0; i < N; i++) sorted[i] = sortable_to_i64(ukeys[i]);

    CHECK(sorted[0] == INT64_MIN, "sort i64: min=%lld", (long long)sorted[0]);
    CHECK(sorted[N-1] == INT64_MAX, "sort i64: max=%lld", (long long)sorted[N-1]);
}

static void test_sort_already_sorted() {
    const size_t N = 1000;
    std::vector<uint32_t> keys(N);
    std::vector<uint32_t> indices(N);
    for (size_t i = 0; i < N; i++) {
        keys[i] = (uint32_t)i;
        indices[i] = (uint32_t)i;
    }

    metal_status st = metal_sort_kv_u32(keys.data(), indices.data(), N);
    CHECK(st == METAL_OK, "already sorted status=%d", st);
    bool correct = true;
    for (size_t i = 0; i < N; i++) {
        if (keys[i] != (uint32_t)i) { correct = false; break; }
    }
    CHECK(correct, "already sorted: unchanged");
}

int main() {
    printf("=== Metal Backend Sort Tests ===\n\n");

    setenv("PGACCEL_METALLIB_PATH", "pgaccel_kernels.metallib", 1);

    metal_status st = metal_init();
    if (st != METAL_OK) {
        fprintf(stderr, "metal_init failed: %d\n", st);
        return 1;
    }

    metal_device_info info = metal_get_device_info();
    printf("Device: %s (unified=%d)\n\n", info.device_name, info.is_unified_memory);

    test_sort_small_u32();
    test_sort_large_u32();
    test_sort_stability_u32();
    test_sort_i32_via_u32();
    test_sort_f32_via_u32();
    test_sort_large_u64();
    test_sort_i64_via_u64();
    test_sort_already_sorted();

    printf("\n=== Results: %d failures ===\n", failures);
    metal_shutdown();
    return failures > 0 ? 1 : 0;
}

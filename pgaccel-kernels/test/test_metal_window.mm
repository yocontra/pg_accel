// test_metal_window — Tests the native Metal backend window function kernels.

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

static void test_row_number_single_partition() {
    // 5 rows, one partition
    uint8_t part[] = {1, 0, 0, 0, 0};
    int64_t results[5] = {};
    metal_status st = metal_window_row_number(part, 5, results);
    CHECK(st == METAL_OK, "row_number single status=%d", st);
    CHECK(results[0] == 1 && results[1] == 2 && results[2] == 3 &&
          results[3] == 4 && results[4] == 5,
          "row_number single: [%lld,%lld,%lld,%lld,%lld]",
          (long long)results[0], (long long)results[1], (long long)results[2],
          (long long)results[3], (long long)results[4]);
}

static void test_row_number_multi_partition() {
    // 8 rows, 3 partitions: [0,1,2] [3,4] [5,6,7]
    uint8_t part[] = {1, 0, 0, 1, 0, 1, 0, 0};
    int64_t results[8] = {};
    metal_status st = metal_window_row_number(part, 8, results);
    CHECK(st == METAL_OK, "row_number multi status=%d", st);
    CHECK(results[0] == 1 && results[1] == 2 && results[2] == 3,
          "row_number partition 1: [%lld,%lld,%lld]",
          (long long)results[0], (long long)results[1], (long long)results[2]);
    CHECK(results[3] == 1 && results[4] == 2,
          "row_number partition 2: [%lld,%lld]",
          (long long)results[3], (long long)results[4]);
    CHECK(results[5] == 1 && results[6] == 2 && results[7] == 3,
          "row_number partition 3: [%lld,%lld,%lld]",
          (long long)results[5], (long long)results[6], (long long)results[7]);
}

static void test_lag_basic() {
    // 5 rows, one partition, lag(offset=1, default=0.0)
    uint8_t part[] = {1, 0, 0, 0, 0};
    double values[] = {10.0, 20.0, 30.0, 40.0, 50.0};
    double results[5] = {};
    metal_status st = metal_window_lag(part, values, nullptr, 5, 1, 0.0,
                                       results, nullptr);
    CHECK(st == METAL_OK, "lag basic status=%d", st);
    CHECK(results[0] == 0.0, "lag[0]=%f (expected 0.0, default)", results[0]);
    CHECK(results[1] == 10.0, "lag[1]=%f (expected 10.0)", results[1]);
    CHECK(results[4] == 40.0, "lag[4]=%f (expected 40.0)", results[4]);
}

static void test_lag_partition_boundary() {
    // 6 rows, 2 partitions: [0,1,2] [3,4,5], lag(offset=2, default=-1.0)
    uint8_t part[] = {1, 0, 0, 1, 0, 0};
    double values[] = {10.0, 20.0, 30.0, 40.0, 50.0, 60.0};
    double results[6] = {};
    metal_status st = metal_window_lag(part, values, nullptr, 6, 2, -1.0,
                                       results, nullptr);
    CHECK(st == METAL_OK, "lag partition status=%d", st);
    // Partition 1: lag(2) of row 0,1 → default (-1.0), row 2 → values[0]=10.0
    CHECK(results[0] == -1.0, "lag part1[0]=%f (expected -1.0)", results[0]);
    CHECK(results[1] == -1.0, "lag part1[1]=%f (expected -1.0)", results[1]);
    CHECK(results[2] == 10.0, "lag part1[2]=%f (expected 10.0)", results[2]);
    // Partition 2: lag(2) of row 3,4 → default, row 5 → values[3]=40.0
    CHECK(results[3] == -1.0, "lag part2[3]=%f (expected -1.0)", results[3]);
    CHECK(results[4] == -1.0, "lag part2[4]=%f (expected -1.0)", results[4]);
    CHECK(results[5] == 40.0, "lag part2[5]=%f (expected 40.0)", results[5]);
}

static void test_lead_basic() {
    // 5 rows, one partition, lead(offset=1, default=99.0)
    uint8_t part[] = {1, 0, 0, 0, 0};
    double values[] = {10.0, 20.0, 30.0, 40.0, 50.0};
    double results[5] = {};
    metal_status st = metal_window_lead(part, values, nullptr, 5, 1, 99.0,
                                        results, nullptr);
    CHECK(st == METAL_OK, "lead basic status=%d", st);
    CHECK(results[0] == 20.0, "lead[0]=%f (expected 20.0)", results[0]);
    CHECK(results[3] == 50.0, "lead[3]=%f (expected 50.0)", results[3]);
    CHECK(results[4] == 99.0, "lead[4]=%f (expected 99.0, default)", results[4]);
}

static void test_lead_partition_boundary() {
    // 6 rows, 2 partitions: [0,1,2] [3,4,5], lead(offset=2, default=-1.0)
    uint8_t part[] = {1, 0, 0, 1, 0, 0};
    double values[] = {10.0, 20.0, 30.0, 40.0, 50.0, 60.0};
    double results[6] = {};
    metal_status st = metal_window_lead(part, values, nullptr, 6, 2, -1.0,
                                        results, nullptr);
    CHECK(st == METAL_OK, "lead partition status=%d", st);
    CHECK(results[0] == 30.0, "lead part1[0]=%f (expected 30.0)", results[0]);
    CHECK(results[1] == -1.0, "lead part1[1]=%f (expected -1.0)", results[1]);
    CHECK(results[2] == -1.0, "lead part1[2]=%f (expected -1.0)", results[2]);
    CHECK(results[3] == 60.0, "lead part2[3]=%f (expected 60.0)", results[3]);
    CHECK(results[4] == -1.0, "lead part2[4]=%f (expected -1.0)", results[4]);
    CHECK(results[5] == -1.0, "lead part2[5]=%f (expected -1.0)", results[5]);
}

static void test_lag_with_nulls() {
    uint8_t part[] = {1, 0, 0, 0};
    double values[] = {10.0, 20.0, 30.0, 40.0};
    uint8_t nulls[] = {0, 1, 0, 0};  // row 1 is NULL
    double results[4] = {};
    uint8_t rnulls[4] = {};
    metal_status st = metal_window_lag(part, values, nulls, 4, 1, 0.0,
                                       results, rnulls);
    CHECK(st == METAL_OK, "lag nulls status=%d", st);
    // lag(1) of row 2 → source is row 1, which is NULL → default + null flag
    CHECK(rnulls[2] == 1, "lag nulls: row 2 result_null=%d (expected 1)", rnulls[2]);
    CHECK(results[2] == 0.0, "lag nulls: row 2 result=%f (expected 0.0)", results[2]);
}

int main() {
    printf("=== Metal Backend Window Tests ===\n\n");

    setenv("PGACCEL_METALLIB_PATH", "pgaccel_kernels.metallib", 1);

    metal_status st = metal_init();
    if (st != METAL_OK) {
        fprintf(stderr, "metal_init failed: %d\n", st);
        return 1;
    }

    metal_device_info info = metal_get_device_info();
    printf("Device: %s (unified=%d)\n\n", info.device_name, info.is_unified_memory);

    test_row_number_single_partition();
    test_row_number_multi_partition();
    test_lag_basic();
    test_lag_partition_boundary();
    test_lead_basic();
    test_lead_partition_boundary();
    test_lag_with_nulls();

    printf("\n=== Results: %d failures ===\n", failures);
    metal_shutdown();
    return failures > 0 ? 1 : 0;
}

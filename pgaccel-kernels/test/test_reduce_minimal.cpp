// Minimal test: does sycl::reduction() actually work on Metal?
#include "pgaccel_ffi.h"
#include <cstdio>

int main() {
    pgaccel_status st = pgaccel_init();
    if (st != PGACCEL_OK) {
        fprintf(stderr, "pgaccel_init failed: %d\n", st);
        return 1;
    }

    // Test 1: heap-allocated data (what actual callers use)
    const size_t N = 1000;
    float* data = new float[N];
    for (size_t i = 0; i < N; i++) data[i] = 25.0f;

    float sum = -1.0f;
    st = pgaccel_reduce_sum_f32(data, N, &sum);
    printf("heap N=%zu: status=%d sum=%f (expected 25000.0)\n", N, st, sum);

    // Test 2: small count
    float small_data[4] = {1.0f, 2.0f, 3.0f, 4.0f};
    float small_sum = -1.0f;
    st = pgaccel_reduce_sum_f32(small_data, 4, &small_sum);
    printf("small N=4: status=%d sum=%f (expected 10.0)\n", st, small_sum);

    // Test 3: count=1 (should skip GPU)
    float one = 42.0f;
    float one_sum = -1.0f;
    st = pgaccel_reduce_sum_f32(&one, 1, &one_sum);
    printf("N=1: status=%d sum=%f (expected 42.0)\n", st, one_sum);

    // Test 4: large count
    const size_t BIG = 1000000;
    float* big = new float[BIG];
    for (size_t i = 0; i < BIG; i++) big[i] = 1.0f;
    float big_sum = -1.0f;
    st = pgaccel_reduce_sum_f32(big, BIG, &big_sum);
    printf("big N=%zu: status=%d sum=%f (expected 1000000.0)\n", BIG, st, big_sum);

    delete[] data;
    delete[] big;
    pgaccel_shutdown();
    return 0;
}

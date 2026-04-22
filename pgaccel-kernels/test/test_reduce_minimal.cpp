// Minimal test: does sycl::reduction() actually work on Metal?
#include <cstdio>

#include "pgaccel_ffi.h"

int main() {
  pgaccel_status st = pgaccel_init();
  if (st != PGACCEL_OK) {
    fprintf(stderr, "pgaccel_init failed: %d\n", st);
    return 1;
  }

  // Test 1: heap-allocated data (what actual callers use)
  const size_t N = 1000;
  float* data = new float[N];
  for (size_t i = 0; i < N; i++)
    data[i] = 25.0f;

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
  for (size_t i = 0; i < BIG; i++)
    big[i] = 1.0f;
  float big_sum = -1.0f;
  st = pgaccel_reduce_sum_f32(big, BIG, &big_sum);
  printf("big N=%zu: status=%d sum=%f (expected 1000000.0)\n", BIG, st, big_sum);

  // Test 5: fused multi-reduce f32 (Fix Agent 4, 2026-04-11).
  // Input: [1, 2, 3, ..., 100] → sum=5050, min=1, max=100, count=100.
  const size_t M = 100;
  float* ramp = new float[M];
  for (size_t i = 0; i < M; ++i)
    ramp[i] = static_cast<float>(i + 1);
  float msum = 0.0f, mmin = 0.0f, mmax = 0.0f;
  int64_t mcount = 0;
  st = pgaccel_reduce_multi_f32(ramp, M, &msum, &mmin, &mmax, &mcount);
  printf("multi_f32 N=%zu: status=%d sum=%f min=%f max=%f count=%lld "
         "(expected 5050, 1, 100, 100)\n",
         M, st, msum, mmin, mmax, static_cast<long long>(mcount));
  delete[] ramp;

  // Test 6: multi-reduce on 1M elements, all 1.0 → sum=1e6, min=1, max=1, count=1e6.
  const size_t MB = 1000000;
  float* ones = new float[MB];
  for (size_t i = 0; i < MB; ++i)
    ones[i] = 1.0f;
  msum = mmin = mmax = 0.0f;
  mcount = 0;
  st = pgaccel_reduce_multi_f32(ones, MB, &msum, &mmin, &mmax, &mcount);
  printf("multi_f32 N=%zu: status=%d sum=%f min=%f max=%f count=%lld "
         "(expected 1000000, 1, 1, 1000000)\n",
         MB, st, msum, mmin, mmax, static_cast<long long>(mcount));
  delete[] ones;

  // Test 7: fused multi-reduce i64.
  const size_t I = 1000;
  int64_t* ints = new int64_t[I];
  for (size_t i = 0; i < I; ++i)
    ints[i] = static_cast<int64_t>(i);
  int64_t isum = 0, imin = 0, imax = 0, icount = 0;
  st = pgaccel_reduce_multi_i64(ints, I, &isum, &imin, &imax, &icount);
  printf("multi_i64 N=%zu: status=%d sum=%lld min=%lld max=%lld count=%lld "
         "(expected 499500, 0, 999, 1000)\n",
         I, st, static_cast<long long>(isum), static_cast<long long>(imin),
         static_cast<long long>(imax), static_cast<long long>(icount));
  delete[] ints;

  delete[] data;
  delete[] big;
  pgaccel_shutdown();
  return 0;
}

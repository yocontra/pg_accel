// Minimal test: does sycl::reduction() actually work on Metal?
#include <cmath>
#include <cstdio>
#include <cstring>
#include <limits>
#include <type_traits>

#include "pgaccel_ffi.h"

namespace {

template <typename T>
using ReduceMultiMaskedFn = pgaccel_status (*)(const T*, const uint8_t*, const uint8_t*, size_t, T*,
                                               T*, T*, int64_t*);

template <typename T>
bool value_matches(T got, T expected) {
  if constexpr (std::is_same_v<T, float>) {
    if (std::isnan(expected))
      return std::isnan(got);
    if (std::isinf(expected))
      return got == expected;
    return std::fabs(got - expected) <= 1.0e-5f;
  } else if constexpr (std::is_same_v<T, double>) {
    if (std::isnan(expected))
      return std::isnan(got);
    if (std::isinf(expected))
      return got == expected;
    return std::fabs(got - expected) <= 1.0e-12;
  } else {
    return got == expected;
  }
}

template <typename T>
void print_value(FILE* stream, T value) {
  if constexpr (std::is_same_v<T, float> || std::is_same_v<T, double>) {
    std::fprintf(stream, "%.17g", static_cast<double>(value));
  } else {
    std::fprintf(stream, "%lld", static_cast<long long>(value));
  }
}

template <typename T>
bool run_masked_case(const char* label, ReduceMultiMaskedFn<T> fn, const T* data,
                     const uint8_t* value_nulls, const uint8_t* selection, size_t count,
                     T expected_sum, T expected_min, T expected_max, int64_t expected_count) {
  T sum = T{-999};
  T min = T{-999};
  T max = T{-999};
  int64_t out_count = -1;

  pgaccel_status st = fn(data, value_nulls, selection, count, &sum, &min, &max, &out_count);
  if (st != PGACCEL_OK) {
    std::fprintf(stderr, "FAIL %s status=%d\n", label, static_cast<int>(st));
    return false;
  }

  bool ok = value_matches(sum, expected_sum) && value_matches(min, expected_min) &&
            value_matches(max, expected_max) && out_count == expected_count;
  if (!ok) {
    std::fprintf(stderr, "FAIL %s got sum=", label);
    print_value(stderr, sum);
    std::fprintf(stderr, " min=");
    print_value(stderr, min);
    std::fprintf(stderr, " max=");
    print_value(stderr, max);
    std::fprintf(stderr, " count=%lld expected sum=", static_cast<long long>(out_count));
    print_value(stderr, expected_sum);
    std::fprintf(stderr, " min=");
    print_value(stderr, expected_min);
    std::fprintf(stderr, " max=");
    print_value(stderr, expected_max);
    std::fprintf(stderr, " count=%lld\n", static_cast<long long>(expected_count));
    return false;
  }

  std::printf("PASS %s\n", label);
  return true;
}

bool run_masked_f32_minmax_case(const char* label, const float* data, const uint8_t* value_nulls,
                                const uint8_t* selection, size_t count, float expected_min,
                                float expected_max, int64_t expected_count) {
  float sum = -999.0f;
  float min = -999.0f;
  float max = -999.0f;
  int64_t out_count = -1;

  pgaccel_status st = pgaccel_reduce_multi_masked_f32(data, value_nulls, selection, count, &sum,
                                                      &min, &max, &out_count);
  if (st != PGACCEL_OK) {
    std::fprintf(stderr, "FAIL %s status=%d\n", label, static_cast<int>(st));
    return false;
  }

  bool ok = value_matches(min, expected_min) && value_matches(max, expected_max) &&
            out_count == expected_count;
  if (!ok) {
    std::fprintf(stderr, "FAIL %s got min=", label);
    print_value(stderr, min);
    std::fprintf(stderr, " max=");
    print_value(stderr, max);
    std::fprintf(stderr, " count=%lld expected min=", static_cast<long long>(out_count));
    print_value(stderr, expected_min);
    std::fprintf(stderr, " max=");
    print_value(stderr, expected_max);
    std::fprintf(stderr, " count=%lld\n", static_cast<long long>(expected_count));
    return false;
  }

  std::printf("PASS %s\n", label);
  return true;
}

int run_masked_f32_special_value_suite() {
  const float inf = std::numeric_limits<float>::infinity();
  const float nan = std::numeric_limits<float>::quiet_NaN();

  int failures = 0;
  const float infinities[3] = {-inf, 5.0f, inf};
  failures += !run_masked_f32_minmax_case("masked_f32 minmax infinities", infinities, nullptr,
                                          nullptr, 3, -inf, inf, 3);

  const float with_nan[3] = {1.0f, nan, 2.0f};
  failures += !run_masked_f32_minmax_case("masked_f32 minmax pg_nan_order", with_nan, nullptr,
                                          nullptr, 3, 1.0f, nan, 3);

  const uint8_t hide_middle[3] = {1, 0, 1};
  failures += !run_masked_f32_minmax_case("masked_f32 selection hides_nan", with_nan, nullptr,
                                          hide_middle, 3, 1.0f, 2.0f, 2);

  const uint8_t null_middle[3] = {0, 1, 0};
  failures += !run_masked_f32_minmax_case("masked_f32 nulls hide_nan", with_nan, null_middle,
                                          nullptr, 3, 1.0f, 2.0f, 2);

  return failures;
}

template <typename T>
int run_masked_suite(const char* type_name, ReduceMultiMaskedFn<T> fn) {
  const T data[8] = {T{5}, T{-2}, T{7}, T{10}, T{-4}, T{3}, T{8}, T{1}};
  const uint8_t selection[8] = {1, 0, 1, 0, 1, 0, 0, 1};
  const uint8_t value_nulls[8] = {0, 1, 0, 0, 0, 1, 1, 1};
  const uint8_t none_selected[8] = {0, 0, 0, 0, 0, 0, 0, 0};

  int failures = 0;
  char label[96];

  std::snprintf(label, sizeof(label), "masked_%s all-selected optional masks", type_name);
  failures += !run_masked_case(label, fn, data, nullptr, nullptr, 8, T{28}, T{-4}, T{10}, 8);

  std::snprintf(label, sizeof(label), "masked_%s filtered rows", type_name);
  failures += !run_masked_case(label, fn, data, nullptr, selection, 8, T{9}, T{-4}, T{7}, 4);

  std::snprintf(label, sizeof(label), "masked_%s null rows", type_name);
  failures += !run_masked_case(label, fn, data, value_nulls, nullptr, 8, T{18}, T{-4}, T{10}, 4);

  std::snprintf(label, sizeof(label), "masked_%s empty effective input", type_name);
  failures += !run_masked_case(label, fn, data, nullptr, none_selected, 8, T{0}, T{0}, T{0}, 0);

  return failures;
}

}  // namespace

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

  // Test 5: fused multi-reduce f32.
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

  int64_t direct_min = 0, direct_max = 0;
  st = pgaccel_reduce_min_i64(ints, I, &direct_min);
  printf("min_i64 N=%zu: status=%d min=%lld (expected 0)\n", I, st,
         static_cast<long long>(direct_min));
  st = pgaccel_reduce_max_i64(ints, I, &direct_max);
  printf("max_i64 N=%zu: status=%d max=%lld (expected 999)\n", I, st,
         static_cast<long long>(direct_max));
  delete[] ints;

  int failures = 0;
  failures += run_masked_suite<float>("f32", pgaccel_reduce_multi_masked_f32);
  failures += run_masked_f32_special_value_suite();
  failures += run_masked_suite<int64_t>("i64", pgaccel_reduce_multi_masked_i64);

  const pgaccel_platform_caps caps = pgaccel_get_caps();
  if (std::strcmp(caps.backend_name, "metal") == 0) {
    double dsum = 0.0, dmin = 0.0, dmax = 0.0;
    int64_t dcount = 0;
    double ddata[2] = {1.0, 2.0};
    st = pgaccel_reduce_multi_masked_f64(ddata, nullptr, nullptr, 2, &dsum, &dmin, &dmax, &dcount);
    if (st != PGACCEL_UNSUPPORTED) {
      std::fprintf(stderr, "FAIL masked_f64 metal unsupported status=%d\n", static_cast<int>(st));
      ++failures;
    } else {
      std::printf("SKIP masked_f64 suites on Metal soft-fp64 struct path\n");
    }
  } else {
    failures += run_masked_suite<double>("f64", pgaccel_reduce_multi_masked_f64);
  }

  delete[] data;
  delete[] big;
  pgaccel_shutdown();
  return failures == 0 ? 0 : 1;
}

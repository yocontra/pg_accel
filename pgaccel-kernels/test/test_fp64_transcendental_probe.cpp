#include <sycl/sycl.hpp>

#include <cmath>
#include <cstdio>
#include <cstring>

#include "pgaccel_ffi.h"

extern sycl::queue* g_queue;

#ifndef PGACCEL_FP64_PROBE_OP
#define PGACCEL_FP64_PROBE_OP 0
#endif

#ifndef PGACCEL_FP64_PROBE_LABEL
#define PGACCEL_FP64_PROBE_LABEL "fp64_probe"
#endif

static bool near(double got, double expected, double abs_tol, double rel_tol) {
  const double diff = std::fabs(got - expected);
  if (diff <= abs_tol)
    return true;
  return diff <= rel_tol * std::fabs(expected);
}

int main() {
  const pgaccel_status init = pgaccel_init();
  if (init != PGACCEL_OK) {
    std::fprintf(stderr, "%s: pgaccel_init failed: %d\n", PGACCEL_FP64_PROBE_LABEL, init);
    return 2;
  }

  sycl::queue* q = g_queue;
  if (q == nullptr) {
    std::fprintf(stderr, "%s: no SYCL queue\n", PGACCEL_FP64_PROBE_LABEL);
    return 2;
  }

  constexpr size_t N = 8;
  double* in_a = sycl::malloc_shared<double>(N * 2, *q);
  double* in_b = sycl::malloc_shared<double>(N * 2, *q);
  double* out = sycl::malloc_shared<double>(N, *q);
  if (in_a == nullptr || in_b == nullptr || out == nullptr) {
    std::fprintf(stderr, "%s: allocation failed\n", PGACCEL_FP64_PROBE_LABEL);
    sycl::free(in_a, *q);
    sycl::free(in_b, *q);
    sycl::free(out, *q);
    return 2;
  }

  const double a_init[N * 2] = {
      0.0,      0.0,       0.125,    -0.125, 0.5,       -0.5,     1.0,      -1.0,
      2.0,      -2.0,      10.0,     -10.0,  0.785398,  -0.785398, 1.2345,   -1.2345,
  };
  const double b_init[N * 2] = {
      1.0, 2.0, 1.25, 2.25, 1.5, 2.5, 1.75, 2.75,
      2.0, 3.0, 2.25, 3.25, 2.5, 3.5, 2.75, 3.75,
  };
  std::memcpy(in_a, a_init, sizeof(a_init));
  std::memcpy(in_b, b_init, sizeof(b_init));
  std::memset(out, 0, N * sizeof(double));

  try {
    q->submit([&](sycl::handler& h) {
       h.parallel_for(sycl::range<1>(N), [=](sycl::id<1> idx) {
         const size_t i = idx[0];
         const double x = in_a[i];
         const double y = in_b[i];

#if PGACCEL_FP64_PROBE_OP == 0
         out[i] = x + y;
#elif PGACCEL_FP64_PROBE_OP == 1
         out[i] = x * y;
#elif PGACCEL_FP64_PROBE_OP == 2
         out[i] = sycl::sqrt(y);
#elif PGACCEL_FP64_PROBE_OP == 3
         out[i] = sycl::sin(x);
#elif PGACCEL_FP64_PROBE_OP == 4
         out[i] = sycl::cos(x);
#elif PGACCEL_FP64_PROBE_OP == 5
         out[i] = sycl::asin(x * 0.5);
#elif PGACCEL_FP64_PROBE_OP == 6
         out[i] = sycl::atan2(x, y);
#elif PGACCEL_FP64_PROBE_OP == 7
         constexpr double deg_to_rad = 3.14159265358979323846264338327950288 / 180.0;
         constexpr double earth_radius = 6371008.8;
         const double lon1 = in_a[i * 2] * deg_to_rad;
         const double lat1 = in_a[i * 2 + 1] * deg_to_rad;
         const double lon2 = in_b[i * 2] * deg_to_rad;
         const double lat2 = in_b[i * 2 + 1] * deg_to_rad;
         const double dlat = lat2 - lat1;
         const double dlon = lon2 - lon1;
         const double sin_dlat = sycl::sin(dlat * 0.5);
         const double sin_dlon = sycl::sin(dlon * 0.5);
         double hv = sin_dlat * sin_dlat + sycl::cos(lat1) * sycl::cos(lat2) * sin_dlon * sin_dlon;
         if (hv < 0.0)
           hv = 0.0;
         if (hv > 1.0)
           hv = 1.0;
         out[i] = earth_radius * (2.0 * sycl::asin(sycl::sqrt(hv)));
#else
         out[i] = x;
#endif
       });
     }).wait_and_throw();
  } catch (const sycl::exception& e) {
    std::fprintf(stderr, "%s: SYCL exception: %s\n", PGACCEL_FP64_PROBE_LABEL, e.what());
    return 3;
  }

  int failures = 0;
  for (size_t i = 0; i < N; ++i) {
    const double x = in_a[i];
    const double y = in_b[i];
    double expected = 0.0;

#if PGACCEL_FP64_PROBE_OP == 0
    expected = x + y;
#elif PGACCEL_FP64_PROBE_OP == 1
    expected = x * y;
#elif PGACCEL_FP64_PROBE_OP == 2
    expected = std::sqrt(y);
#elif PGACCEL_FP64_PROBE_OP == 3
    expected = std::sin(x);
#elif PGACCEL_FP64_PROBE_OP == 4
    expected = std::cos(x);
#elif PGACCEL_FP64_PROBE_OP == 5
    expected = std::asin(x * 0.5);
#elif PGACCEL_FP64_PROBE_OP == 6
    expected = std::atan2(x, y);
#elif PGACCEL_FP64_PROBE_OP == 7
    constexpr double deg_to_rad = 3.14159265358979323846264338327950288 / 180.0;
    constexpr double earth_radius = 6371008.8;
    const double lon1 = in_a[i * 2] * deg_to_rad;
    const double lat1 = in_a[i * 2 + 1] * deg_to_rad;
    const double lon2 = in_b[i * 2] * deg_to_rad;
    const double lat2 = in_b[i * 2 + 1] * deg_to_rad;
    const double dlat = lat2 - lat1;
    const double dlon = lon2 - lon1;
    const double sin_dlat = std::sin(dlat * 0.5);
    const double sin_dlon = std::sin(dlon * 0.5);
    double hv = sin_dlat * sin_dlat + std::cos(lat1) * std::cos(lat2) * sin_dlon * sin_dlon;
    hv = std::max(0.0, std::min(1.0, hv));
    expected = earth_radius * (2.0 * std::asin(std::sqrt(hv)));
#else
    expected = x;
#endif

    if (!near(out[i], expected, 1e-9, 1e-9)) {
      std::printf("%s row %zu mismatch: got %.17g expected %.17g\n", PGACCEL_FP64_PROBE_LABEL, i,
                  out[i], expected);
      ++failures;
    }
  }

  std::printf("%s: completed with %d mismatches\n", PGACCEL_FP64_PROBE_LABEL, failures);

  sycl::free(in_a, *q);
  sycl::free(in_b, *q);
  sycl::free(out, *q);
  pgaccel_shutdown();
  return failures == 0 ? 0 : 1;
}

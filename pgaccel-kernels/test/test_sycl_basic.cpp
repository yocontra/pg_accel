// Basic SYCL runtime diagnostics used by the Metal backend smoke chain.
//
// Raw host pointers are intentionally not a supported kernel contract. Metal on
// Apple Silicon can silently read zeros from raw host pointers, so production
// kernels must stage inputs through SYCL allocations.
#include <sycl/sycl.hpp>

#include <cmath>
#include <cstdio>

#include "pgaccel_ffi.h"

extern sycl::queue* g_queue;

namespace {

bool nearly_eq(float a, float b) {
  return std::fabs(a - b) < 1e-5f;
}

bool assert_float(const char* name, float got, float expected) {
  if (!nearly_eq(got, expected)) {
    std::fprintf(stderr, "%s: got %.6f, expected %.6f\n", name, got, expected);
    return false;
  }
  return true;
}

}  // namespace

int main() {
  pgaccel_init();

  sycl::queue* q = g_queue;
  if (q == nullptr) {
    std::fprintf(stderr, "No SYCL queue\n");
    return 1;
  }

  bool ok = true;
  {
    float* out = sycl::malloc_shared<float>(1, *q);
    if (out == nullptr)
      return 1;
    *out = -1.0f;
    q->submit([&](sycl::handler& h) { h.single_task([=]() { *out = 42.0f; }); }).wait();
    std::printf("Test 1 (single_task write): %.0f (expected 42)\n", *out);
    ok &= assert_float("single_task shared write", *out, 42.0f);
    sycl::free(out, *q);
  }

  {
    constexpr size_t N = 8;
    float* out = sycl::malloc_shared<float>(N, *q);
    if (out == nullptr)
      return 1;
    for (size_t i = 0; i < N; ++i)
      out[i] = -1.0f;
    q->submit([&](sycl::handler& h) {
       h.parallel_for(sycl::range<1>(N),
                      [=](sycl::id<1> i) { out[i] = static_cast<float>(i[0]) + 1.0f; });
     }).wait();
    std::printf("Test 2 (parallel_for write):");
    for (size_t i = 0; i < N; ++i) {
      std::printf(" %.0f", out[i]);
      ok &= assert_float("parallel_for shared write", out[i], static_cast<float>(i + 1));
    }
    std::printf(" (expected 1 2 3 4 5 6 7 8)\n");
    sycl::free(out, *q);
  }

  {
    constexpr size_t N = 8;
    float* in = sycl::malloc_shared<float>(N, *q);
    float* out = sycl::malloc_shared<float>(N, *q);
    if (in == nullptr || out == nullptr)
      return 1;
    for (size_t i = 0; i < N; ++i) {
      in[i] = 10.0f;
      out[i] = -1.0f;
    }
    q->submit([&](sycl::handler& h) {
       h.parallel_for(sycl::range<1>(N), [=](sycl::id<1> i) { out[i] = in[i] * 2.0f; });
     }).wait();
    std::printf("Test 3 (read+write shared):");
    for (size_t i = 0; i < N; ++i) {
      std::printf(" %.0f", out[i]);
      ok &= assert_float("parallel_for shared read/write", out[i], 20.0f);
    }
    std::printf(" (expected 20 20 20 20 20 20 20 20)\n");
    sycl::free(in, *q);
    sycl::free(out, *q);
  }

  {
    constexpr size_t N = 8;
    float* out = sycl::malloc_shared<float>(1, *q);
    float* in = sycl::malloc_shared<float>(N, *q);
    if (in == nullptr || out == nullptr)
      return 1;
    for (size_t i = 0; i < N; ++i)
      in[i] = 1.0f;
    *out = -1.0f;
    q->submit([&](sycl::handler& h) {
       sycl::local_accessor<float, 1> lm(N, h);
       h.parallel_for(sycl::nd_range<1>(N, N), [=](sycl::nd_item<1> item) {
         const size_t lid = item.get_local_id(0);
         lm[lid] = in[lid];
         item.barrier(sycl::access::fence_space::local_space);
         for (size_t s = N / 2; s > 0; s >>= 1) {
           if (lid < s)
             lm[lid] += lm[lid + s];
           item.barrier(sycl::access::fence_space::local_space);
         }
         if (lid == 0)
           *out = lm[0];
       });
     }).wait();
    std::printf("Test 5 (nd_range tree reduce): %.0f (expected 8)\n", *out);
    ok &= assert_float("nd_range tree reduce", *out, 8.0f);
    sycl::free(in, *q);
    sycl::free(out, *q);
  }

  {
    constexpr size_t N = 8;
    float* in = sycl::malloc_shared<float>(N, *q);
    float* out = sycl::malloc_shared<float>(1, *q);
    if (in == nullptr || out == nullptr)
      return 1;
    for (size_t i = 0; i < N; ++i)
      in[i] = 1.0f;
    *out = 0.0f;
    q->submit([&](sycl::handler& h) {
       auto red = sycl::reduction(out, sycl::plus<float>());
       h.parallel_for(sycl::range<1>(N), red, [=](sycl::id<1> i, auto& sum) { sum += in[i]; });
     }).wait();
    std::printf("Test 6 (sycl::reduction): %.0f (expected 8)\n", *out);
    ok &= assert_float("sycl::reduction", *out, 8.0f);
    sycl::free(in, *q);
    sycl::free(out, *q);
  }

  pgaccel_shutdown();
  return ok ? 0 : 1;
}

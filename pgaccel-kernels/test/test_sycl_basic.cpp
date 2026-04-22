// Diagnose which SYCL features work on Metal.
#include <sycl/sycl.hpp>

#include <cstdio>

#include "pgaccel_ffi.h"
extern sycl::queue* g_queue;
extern bool g_unified_memory;

int main() {
  pgaccel_init();

  sycl::queue* q = g_queue;
  if (!q) {
    fprintf(stderr, "No queue\n");
    return 1;
  }
  printf("unified_memory=%d\n", (int)g_unified_memory);

  // Test 1: Can a kernel write to shared memory at all?
  {
    float* out = sycl::malloc_shared<float>(1, *q);
    *out = -1.0f;
    q->submit([&](sycl::handler& h) { h.single_task([=]() { *out = 42.0f; }); }).wait();
    printf("Test 1 (single_task write): %f (expected 42.0)\n", *out);
    sycl::free(out, *q);
  }

  // Test 2: parallel_for writing to shared array
  {
    const size_t N = 8;
    float* out = sycl::malloc_shared<float>(N, *q);
    for (size_t i = 0; i < N; i++)
      out[i] = -1.0f;
    q->submit([&](sycl::handler& h) {
       h.parallel_for(sycl::range<1>(N),
                      [=](sycl::id<1> i) { out[i] = static_cast<float>(i[0]) + 1.0f; });
     }).wait();
    printf("Test 2 (parallel_for write): ");
    for (size_t i = 0; i < N; i++)
      printf("%.0f ", out[i]);
    printf("(expected 1 2 3 4 5 6 7 8)\n");
    sycl::free(out, *q);
  }

  // Test 3: parallel_for reading from shared input + writing output
  {
    const size_t N = 8;
    float* in = sycl::malloc_shared<float>(N, *q);
    float* out = sycl::malloc_shared<float>(N, *q);
    for (size_t i = 0; i < N; i++) {
      in[i] = 10.0f;
      out[i] = -1.0f;
    }
    q->submit([&](sycl::handler& h) {
       h.parallel_for(sycl::range<1>(N), [=](sycl::id<1> i) { out[i] = in[i] * 2.0f; });
     }).wait();
    printf("Test 3 (read+write shared): ");
    for (size_t i = 0; i < N; i++)
      printf("%.0f ", out[i]);
    printf("(expected 20 20 20 20 20 20 20 20)\n");
    sycl::free(in, *q);
    sycl::free(out, *q);
  }

  // Test 4: parallel_for reading from RAW HOST pointer (unified mem assumption)
  {
    const size_t N = 8;
    float host_data[N];
    for (size_t i = 0; i < N; i++)
      host_data[i] = 5.0f;
    float* out = sycl::malloc_shared<float>(N, *q);
    for (size_t i = 0; i < N; i++)
      out[i] = -1.0f;
    float* raw = host_data;  // raw host pointer
    q->submit([&](sycl::handler& h) {
       h.parallel_for(sycl::range<1>(N), [=](sycl::id<1> i) { out[i] = raw[i] * 3.0f; });
     }).wait();
    printf("Test 4 (read raw host ptr): ");
    for (size_t i = 0; i < N; i++)
      printf("%.0f ", out[i]);
    printf("(expected 15 15 15 15 15 15 15 15)\n");
    sycl::free(out, *q);
  }

  // Test 5: nd_range + local_accessor
  {
    const size_t N = 8;
    float* out = sycl::malloc_shared<float>(1, *q);
    float* in = sycl::malloc_shared<float>(N, *q);
    for (size_t i = 0; i < N; i++)
      in[i] = 1.0f;
    *out = -1.0f;
    q->submit([&](sycl::handler& h) {
       sycl::local_accessor<float, 1> lm(N, h);
       h.parallel_for(sycl::nd_range<1>(N, N), [=](sycl::nd_item<1> item) {
         size_t lid = item.get_local_id(0);
         lm[lid] = in[lid];
         item.barrier(sycl::access::fence_space::local_space);
         // Simple tree reduce
         for (size_t s = N / 2; s > 0; s >>= 1) {
           if (lid < s)
             lm[lid] += lm[lid + s];
           item.barrier(sycl::access::fence_space::local_space);
         }
         if (lid == 0)
           *out = lm[0];
       });
     }).wait();
    printf("Test 5 (nd_range tree reduce): %f (expected 8.0)\n", *out);
    sycl::free(in, *q);
    sycl::free(out, *q);
  }

  // Test 6: sycl::reduction (the broken API)
  {
    const size_t N = 8;
    float* in = sycl::malloc_shared<float>(N, *q);
    float* out = sycl::malloc_shared<float>(1, *q);
    for (size_t i = 0; i < N; i++)
      in[i] = 1.0f;
    *out = 0.0f;
    q->submit([&](sycl::handler& h) {
       auto red = sycl::reduction(out, sycl::plus<float>());
       h.parallel_for(sycl::range<1>(N), red, [=](sycl::id<1> i, auto& sum) { sum += in[i]; });
     }).wait();
    printf("Test 6 (sycl::reduction): %f (expected 8.0)\n", *out);
    sycl::free(in, *q);
    sycl::free(out, *q);
  }

  pgaccel_shutdown();
  return 0;
}

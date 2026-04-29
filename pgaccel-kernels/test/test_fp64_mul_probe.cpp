// fp64 multiply lowering probe.
//
// Goal: determine which bit slot is being dropped by the Metal SSCP
// lowering of fp64 mul. soft-fp agent's correct observation: the
// bug cannot be in sf64_mul (4-line ABI wrapper, fully covered by
// existing tests including the v==w diagonal). Bug is in
// AdaptiveCpp/src/compiler/llvm-to-backend/metal/.
//
// This probe: load known-good fp64 inputs from device memory,
// compute v*v in a SYCL kernel, read back, print exact bit
// patterns. Compare against expected bit pattern for each.
#include <sycl/sycl.hpp>

#include <cinttypes>
#include <cstdint>
#include <cstdio>
#include <cstring>

#include "pgaccel_ffi.h"
extern sycl::queue* g_queue;

static uint64_t bits_of(double d) {
  uint64_t u;
  std::memcpy(&u, &d, sizeof(u));
  return u;
}

static void row(const char* label, double got, double expected) {
  uint64_t gb = bits_of(got);
  uint64_t eb = bits_of(expected);
  uint64_t xor_bits = gb ^ eb;
  printf("  %-28s got=%.17g (0x%016" PRIx64 ")  expected=%.17g (0x%016" PRIx64
         ")  xor=0x%016" PRIx64 "  %s\n",
         label, got, gb, expected, eb, xor_bits, gb == eb ? "OK" : "MISMATCH");
}

int main() {
  pgaccel_init();
  sycl::queue* q = g_queue;
  if (!q) {
    fprintf(stderr, "no queue\n");
    return 1;
  }

  const size_t N = 8;
  double* in = sycl::malloc_shared<double>(N, *q);
  double* out_vsq = sycl::malloc_shared<double>(N, *q);
  double* out_vw = sycl::malloc_shared<double>(N, *q);
  double* out_const = sycl::malloc_shared<double>(N, *q);
  double* out_sumadd = sycl::malloc_shared<double>(N, *q);

  // Inputs probe the suspect bit slots.
  in[0] = 1.0;     // 0x3FF0000000000000
  in[1] = 0.5;     // 0x3FE0000000000000
  in[2] = 2.0;     // 0x4000000000000000
  in[3] = 3.0;     // 0x4008000000000000
  in[4] = -100.0;  // 0xC059000000000000 (matches reduce_sum_sq input range)
  in[5] = 1.5;     // 0x3FF8000000000000
  in[6] = 1e-150;  // very small but normal
  in[7] = 4.0;     // 0x4010000000000000

  // Probe A: v * v where both operands are the SAME load (same SSA).
  q->submit([&](sycl::handler& h) {
     h.parallel_for(sycl::range<1>(N), [=](sycl::id<1> i) {
       double v = in[i];
       out_vsq[i] = v * v;
     });
   }).wait();

  // Probe B: v * w where v and w are SEPARATE loads of the same address.
  // This tests whether the bug is sensitive to SSA identity.
  q->submit([&](sycl::handler& h) {
     h.parallel_for(sycl::range<1>(N), [=](sycl::id<1> i) {
       double v = in[i];
       double w = in[i];  // separate load (probably CSE'd, but try)
       out_vw[i] = v * w;
     });
   }).wait();

  // Probe C: v * <constant>. Tests immediate-operand path.
  q->submit([&](sycl::handler& h) {
     h.parallel_for(sycl::range<1>(N), [=](sycl::id<1> i) {
       double v = in[i];
       out_const[i] = v * 2.0;
     });
   }).wait();

  // Probe D: pure add. Per evidence, sum-only kernels work.
  q->submit([&](sycl::handler& h) {
     h.parallel_for(sycl::range<1>(N), [=](sycl::id<1> i) {
       double v = in[i];
       out_sumadd[i] = v + v;
     });
   }).wait();

  printf("\n=== Probe A: v * v (same SSA) ===\n");
  for (size_t i = 0; i < N; i++)
    row("v*v", out_vsq[i], in[i] * in[i]);

  printf("\n=== Probe B: v * w (separate loads of same addr) ===\n");
  for (size_t i = 0; i < N; i++)
    row("v*w", out_vw[i], in[i] * in[i]);

  printf("\n=== Probe C: v * 2.0 (immediate const operand) ===\n");
  for (size_t i = 0; i < N; i++)
    row("v*2.0", out_const[i], in[i] * 2.0);

  printf("\n=== Probe D: v + v (pure add, control) ===\n");
  for (size_t i = 0; i < N; i++)
    row("v+v", out_sumadd[i], in[i] + in[i]);

  // Compute the ratio for the failing case to compare against the
  // 0.555 number from the reduce kernel.
  printf("\n=== Ratio summary (got/expected) ===\n");
  for (size_t i = 0; i < N; i++) {
    double exp = in[i] * in[i];
    double got = out_vsq[i];
    double ratio = (exp == 0.0) ? 0.0 : (got / exp);
    printf("  in=%.17g  v*v_ratio=%.17g  v*w_ratio=%.17g  v*2.0_ratio=%.17g\n", in[i], ratio,
           (exp == 0.0) ? 0.0 : (out_vw[i] / exp),
           (in[i] * 2.0 == 0.0) ? 0.0 : (out_const[i] / (in[i] * 2.0)));
  }

  sycl::free(in, *q);
  sycl::free(out_vsq, *q);
  sycl::free(out_vw, *q);
  sycl::free(out_const, *q);
  sycl::free(out_sumadd, *q);
  pgaccel_shutdown();
  return 0;
}

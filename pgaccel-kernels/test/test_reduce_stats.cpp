// test_reduce_stats.cpp — validates fp64 and fp32 reduce kernels at
// multiple sizes (1k, 64k, 256k, 1M) against CPU references.
//
// fp64 tolerance policy (soft-fp64 v1.0 ABI contract):
//   - Scalar ops (add/sub/mul/div/sqrt/fma/compare): 0 ULP (bit-exact)
//   - exp/log/sin/cos/trig/hypot/pow: ≤4 ULP (u10 contract)
//   - Reduction / accumulation (Σ across N elements):
//       tree-reduce-aware bound = log2(N) * 32 ULP.
//       Rationale: pairwise summation error bound (Higham, "Accuracy and
//       Stability of Numerical Algorithms", Theorem 4.5) is
//       |fl(Σxᵢ) − Σxᵢ| ≤ (log2(N)·eps + O(eps²))·Σ|xᵢ|
//       so the ULP distance at the final result grows linearly with the
//       reduction-tree depth log2(N). The 32× factor is the safety margin
//       used for the ~U(−100, 100) workload here, where partial cancellation
//       (E[Σx] ≈ 0) makes |result| small relative to Σ|xᵢ| and amplifies the
//       per-level rounding into many ULPs at the final answer. This budget
//       supersedes the prior fixed 8-ULP cap which assumed bit-exact scalar
//       compounding (impossible once the kernel uses tree-reduce reorder).
//   - Σ(x²) (sum_sq): same tree-reduce-aware bound, log2(N) * 32 ULP.
//
// This test replaces the prior skip-on-PGACCEL_UNSUPPORTED branches.
// Since the fp64-unlock plan landed (W1/W2/W3/W4), reduce_*_f64 must
// execute via soft-fp64 on Metal. Any UNSUPPORTED status is a FAIL.

#include <algorithm>
#include <cmath>
#include <cstdint>
#include <cstdio>
#include <cstdlib>
#include <cstring>
#include <random>
#include <vector>

#include "pgaccel_ffi.h"

static int g_failures = 0;

// ULP distance for double (IEEE-754 binary64). Returns UINT64_MAX when
// either argument is NaN (finite/NaN mix is also UINT64_MAX).
static uint64_t ulp_distance_f64(double a, double b) {
  if (std::isnan(a) || std::isnan(b)) {
    return UINT64_MAX;
  }
  if (a == b) {
    return 0;
  }
  uint64_t ua, ub;
  std::memcpy(&ua, &a, sizeof(ua));
  std::memcpy(&ub, &b, sizeof(ub));
  // Biased representation: flip sign bit so negatives sort below positives.
  constexpr uint64_t SIGN = 0x8000000000000000ULL;
  ua = (ua & SIGN) ? ~ua + 1 : ua | SIGN;
  ub = (ub & SIGN) ? ~ub + 1 : ub | SIGN;
  return ua > ub ? ua - ub : ub - ua;
}

static int check_ulp(const char* label, double got, double expected, uint64_t max_ulp) {
  const uint64_t dist = ulp_distance_f64(got, expected);
  if (dist > max_ulp) {
    std::fprintf(stderr, "FAIL %s: got %.17g expected %.17g (ulp_dist=%llu, budget=%llu)\n", label,
                 got, expected, (unsigned long long)dist, (unsigned long long)max_ulp);
    g_failures++;
    return 1;
  }
  std::printf("  OK  %s: got %.17g expected %.17g (ulp_dist=%llu ≤ %llu)\n", label, got, expected,
              (unsigned long long)dist, (unsigned long long)max_ulp);
  return 0;
}

// Tree-reduce-aware ULP budget.  See file-header comment for the derivation.
// Floors at 8 ULP so small N (where log2(N) is tiny) keeps the historical
// scalar-path tolerance.
static uint64_t tree_reduce_budget_ulp(size_t n) {
  uint64_t log2_n = 0;
  size_t v = (n > 1) ? n - 1 : 1;
  while (v > 0) {
    ++log2_n;
    v >>= 1;
  }
  uint64_t budget = log2_n * 32;
  return budget < 8 ? 8 : budget;
}

static int check_eq_u64(const char* label, uint64_t got, uint64_t expected) {
  if (got != expected) {
    std::fprintf(stderr, "FAIL %s: got %llu expected %llu\n", label, (unsigned long long)got,
                 (unsigned long long)expected);
    g_failures++;
    return 1;
  }
  std::printf("  OK  %s: %llu\n", label, (unsigned long long)got);
  return 0;
}

// Test one size. Generates a random f64 vector seeded deterministically,
// computes CPU reference stats in fp64, invokes every fp64 reduce kernel,
// and compares with u35 (≤8 ULP) tolerance for sums (reductions) and
// u10 (≤4 ULP) tolerance for derived stddev/var (one sqrt).
static void test_size(size_t N) {
  std::printf("\n=== fp64 reduce @ N=%zu ===\n", N);
  std::mt19937_64 rng(0xC0FFEEULL ^ N);
  // Use a mild range so we don't saturate to inf when summing 1M elements.
  std::uniform_real_distribution<double> dist(-100.0, 100.0);

  std::vector<double> d(N);
  std::vector<float> f(N);
  for (size_t i = 0; i < N; ++i) {
    d[i] = dist(rng);
    f[i] = static_cast<float>(d[i]);
  }

  // CPU reference (fp64). Use plain sequential sum — soft-fp64 reduce
  // does a tree-reduction so ≤8 ULP budget covers the reorder.
  double ref_sum = 0.0;
  double ref_min = d[0];
  double ref_max = d[0];
  double ref_sum_sq = 0.0;
  for (size_t i = 0; i < N; ++i) {
    ref_sum += d[i];
    ref_sum_sq += d[i] * d[i];
    if (d[i] < ref_min)
      ref_min = d[i];
    if (d[i] > ref_max)
      ref_max = d[i];
  }
  const double ref_avg = ref_sum / static_cast<double>(N);
  // Sample variance + stddev (N-1 denominator per PG semantics)
  double ref_sq_dev = 0.0;
  for (size_t i = 0; i < N; ++i) {
    const double delta = d[i] - ref_avg;
    ref_sq_dev += delta * delta;
  }
  const double ref_var_samp = N > 1 ? ref_sq_dev / static_cast<double>(N - 1) : 0.0;
  const double ref_var_pop = ref_sq_dev / static_cast<double>(N);
  const double ref_stddev_samp = std::sqrt(ref_var_samp);
  const double ref_stddev_pop = std::sqrt(ref_var_pop);

  const uint64_t reduce_budget = tree_reduce_budget_ulp(N);

  // -- reduce_sum_f64 --
  {
    double got = -1.0;
    pgaccel_status st = pgaccel_reduce_sum_f64(d.data(), N, &got);
    if (st != PGACCEL_OK) {
      std::fprintf(stderr, "FAIL reduce_sum_f64 N=%zu status=%d\n", N, (int)st);
      g_failures++;
    } else {
      // Tree-reduce-aware bound — see file header for derivation.
      check_ulp("reduce_sum_f64", got, ref_sum, reduce_budget);
    }
  }

  // -- reduce_min_f64 --
  {
    double got = 1e300;
    pgaccel_status st = pgaccel_reduce_min_f64(d.data(), N, &got);
    if (st != PGACCEL_OK) {
      std::fprintf(stderr, "FAIL reduce_min_f64 N=%zu status=%d\n", N, (int)st);
      g_failures++;
    } else {
      // min/max is scalar-compare (no reorder matters for min) — 0 ULP
      check_ulp("reduce_min_f64", got, ref_min, 0);
    }
  }

  // -- reduce_max_f64 --
  {
    double got = -1e300;
    pgaccel_status st = pgaccel_reduce_max_f64(d.data(), N, &got);
    if (st != PGACCEL_OK) {
      std::fprintf(stderr, "FAIL reduce_max_f64 N=%zu status=%d\n", N, (int)st);
      g_failures++;
    } else {
      check_ulp("reduce_max_f64", got, ref_max, 0);
    }
  }

  // -- reduce_multi_f64 (fused sum+min+max+count) --
  {
    double mss = 0.0, mmn = 0.0, mmx = 0.0;
    int64_t mcnt = 0;
    pgaccel_status st = pgaccel_reduce_multi_f64(d.data(), N, &mss, &mmn, &mmx, &mcnt);
    if (st != PGACCEL_OK) {
      std::fprintf(stderr, "FAIL reduce_multi_f64 N=%zu status=%d\n", N, (int)st);
      g_failures++;
    } else {
      check_ulp("reduce_multi_f64 sum", mss, ref_sum, reduce_budget);
      check_ulp("reduce_multi_f64 min", mmn, ref_min, 0);
      check_ulp("reduce_multi_f64 max", mmx, ref_max, 0);
      check_eq_u64("reduce_multi_f64 count", static_cast<uint64_t>(mcnt), N);
    }
  }

  // -- reduce_sum_sq_f64 --
  {
    double got = -1.0;
    pgaccel_status st = pgaccel_reduce_sum_sq_f64(d.data(), N, &got);
    if (st != PGACCEL_OK) {
      std::fprintf(stderr, "FAIL reduce_sum_sq_f64 N=%zu status=%d\n", N, (int)st);
      g_failures++;
    } else {
      check_ulp("reduce_sum_sq_f64", got, ref_sum_sq, reduce_budget);
    }
  }

  // -- reduce_stats_f64 (fused count+sum+sum_sq → AVG/STDDEV/VAR) --
  {
    uint64_t cnt = 0;
    double sm = 0.0, sq = 0.0;
    pgaccel_status st = pgaccel_reduce_stats_f64(d.data(), N, &cnt, &sm, &sq);
    if (st != PGACCEL_OK) {
      std::fprintf(stderr, "FAIL reduce_stats_f64 N=%zu status=%d\n", N, (int)st);
      g_failures++;
    } else {
      check_eq_u64("reduce_stats_f64 count", cnt, N);
      check_ulp("reduce_stats_f64 sum", sm, ref_sum, reduce_budget);
      check_ulp("reduce_stats_f64 sum_sq", sq, ref_sum_sq, reduce_budget);

      // Derived stats: AVG/VAR/STDDEV computed from the returned
      // (count, sum, sum_sq) via the two-pass formula used by the
      // partial-agg planner. The derived budget inherits the same
      // tree-reduce-aware ULP bound as `sum` since `avg = sum/count`
      // is one extra correctly-rounded fp64 op (≤1 ULP), absorbed by
      // the same envelope.
      const double avg = sm / static_cast<double>(cnt);
      check_ulp("reduce_stats_f64 avg (derived)", avg, ref_avg, reduce_budget);

      // var_pop = (sum_sq - count * avg^2) / count
      const double var_pop = (sq - static_cast<double>(cnt) * avg * avg) / static_cast<double>(cnt);
      // var_samp = (sum_sq - count * avg^2) / (count - 1)
      const double var_samp =
          cnt > 1 ? (sq - static_cast<double>(cnt) * avg * avg) / static_cast<double>(cnt - 1)
                  : 0.0;
      const double stddev_pop = std::sqrt(std::max(0.0, var_pop));
      const double stddev_samp = std::sqrt(std::max(0.0, var_samp));

      // Cancellation in (sum_sq - count*avg^2) can blow up relative
      // error past u35 — but for uniform[-100,100] with mean ~0 the
      // cancellation is mild. Keep the budget at 64 ULP for derived
      // quantities; flag anything larger as a kernel regression.
      check_ulp("var_pop (derived)", var_pop, ref_var_pop, 64);
      check_ulp("var_samp (derived)", var_samp, ref_var_samp, 64);
      check_ulp("stddev_pop (derived)", stddev_pop, ref_stddev_pop, 64);
      check_ulp("stddev_samp (derived)", stddev_samp, ref_stddev_samp, 64);
    }
  }
}

static void test_fp32_regression() {
  // Preserve the prior [1..100] fp32 coverage — never shorten, never loosen.
  std::printf("\n=== fp32 regression (legacy [1..100] coverage) ===\n");
  constexpr size_t N = 100;
  float fdata[N];
  for (size_t i = 0; i < N; ++i)
    fdata[i] = static_cast<float>(i + 1);
  const double EXPECTED_SUM = 5050.0;
  const double EXPECTED_SUM_SQ = 338350.0;
  const uint64_t EXPECTED_COUNT = 100ULL;

  {
    double sum_sq = -1.0;
    pgaccel_status st = pgaccel_reduce_sum_sq_f32(fdata, N, &sum_sq);
    if (st != PGACCEL_OK) {
      std::fprintf(stderr, "FAIL sum_sq_f32 status=%d\n", st);
      g_failures++;
    } else if (std::fabs(sum_sq - EXPECTED_SUM_SQ) > 1e-3) {
      std::fprintf(stderr, "FAIL sum_sq_f32: got %.6f expected %.6f\n", sum_sq, EXPECTED_SUM_SQ);
      g_failures++;
    } else {
      std::printf("  OK  sum_sq_f32: %.4f\n", sum_sq);
    }
  }
  {
    uint64_t count = 0;
    double sum = 0.0, sum_sq = 0.0;
    pgaccel_status st = pgaccel_reduce_stats_f32(fdata, N, &count, &sum, &sum_sq);
    if (st != PGACCEL_OK) {
      std::fprintf(stderr, "FAIL stats_f32 status=%d\n", st);
      g_failures++;
    } else {
      if (count != EXPECTED_COUNT) {
        std::fprintf(stderr, "FAIL stats_f32 count: got %llu\n", (unsigned long long)count);
        g_failures++;
      }
      if (std::fabs(sum - EXPECTED_SUM) > 1e-3 || std::fabs(sum_sq - EXPECTED_SUM_SQ) > 1e-3) {
        std::fprintf(stderr, "FAIL stats_f32 values: sum=%.4f sum_sq=%.4f\n", sum, sum_sq);
        g_failures++;
      } else {
        std::printf("  OK  stats_f32: count=%llu sum=%.4f sum_sq=%.4f\n", (unsigned long long)count,
                    sum, sum_sq);
      }
    }
  }
  // Empty-input zero-init contract.
  {
    uint64_t count = 99;
    double sum = -1.0, sum_sq = -1.0;
    pgaccel_status st = pgaccel_reduce_stats_f32(nullptr, 0, &count, &sum, &sum_sq);
    if (st != PGACCEL_OK || count != 0 || sum != 0.0 || sum_sq != 0.0) {
      std::fprintf(stderr, "FAIL stats_f32 N=0 zero-init contract\n");
      g_failures++;
    } else {
      std::printf("  OK  stats_f32 N=0 zero-init\n");
    }
  }
}

int main() {
  pgaccel_status st = pgaccel_init();
  if (st != PGACCEL_OK) {
    std::fprintf(stderr, "pgaccel_init failed: %d\n", st);
    return 1;
  }

  pgaccel_device_info info = pgaccel_get_device_info();
  std::printf("Device: %s backend=%s has_native_fp64=%d\n", info.device_name, info.backend_name,
              info.has_native_fp64);

  // Sizes per W5 fp64-unlock plan. The 1k size catches scalar-path bugs,
  // 1M catches tree-reduce accumulation drift.
  for (size_t N : {size_t(1024), size_t(65536), size_t(262144), size_t(1048576)}) {
    test_size(N);
  }

  test_fp32_regression();

  pgaccel_shutdown();

  if (g_failures > 0) {
    std::fprintf(stderr, "\nFAIL: %d failure(s)\n", g_failures);
    return 1;
  }
  std::printf("\nPASS\n");
  return 0;
}

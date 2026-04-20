// test_reduce_stats.cpp — validates pgaccel_reduce_sum_sq_* and
// pgaccel_reduce_stats_* against [1..100]:
//   count  = 100
//   sum    = 5050
//   sum_sq = 338350  (= 100*101*201/6)
//
// fp32 path runs on every backend (Metal/CUDA/ROCm/L0).
// fp64 path is exercised only when the device reports fp64; otherwise we
// accept PGACCEL_UNSUPPORTED and continue (Metal returns this).

#include "pgaccel_ffi.h"
#include <cstdio>
#include <cstdlib>
#include <cstdint>
#include <cmath>

static int check_close(const char* label, double got, double expected,
                       double tol) {
    if (std::fabs(got - expected) > tol) {
        std::fprintf(stderr, "FAIL %s: got %.6f expected %.6f (tol %.6f)\n",
                     label, got, expected, tol);
        return 1;
    }
    return 0;
}

int main() {
    pgaccel_status st = pgaccel_init();
    if (st != PGACCEL_OK) {
        std::fprintf(stderr, "pgaccel_init failed: %d\n", st);
        return 1;
    }

    constexpr size_t N = 100;
    float fdata[N];
    double ddata[N];
    for (size_t i = 0; i < N; ++i) {
        fdata[i] = static_cast<float>(i + 1);
        ddata[i] = static_cast<double>(i + 1);
    }

    const double EXPECTED_SUM = 5050.0;
    const double EXPECTED_SUM_SQ = 338350.0;
    const uint64_t EXPECTED_COUNT = 100ULL;

    int failures = 0;

    // -- sum_sq f32 --
    {
        double sum_sq = -1.0;
        st = pgaccel_reduce_sum_sq_f32(fdata, N, &sum_sq);
        std::printf("sum_sq_f32 N=%zu: status=%d sum_sq=%.4f\n", N, st, sum_sq);
        if (st != PGACCEL_OK) {
            std::fprintf(stderr, "FAIL sum_sq_f32 status=%d\n", st);
            failures++;
        } else {
            failures += check_close("sum_sq_f32", sum_sq, EXPECTED_SUM_SQ, 1e-3);
        }
    }

    // -- stats f32 --
    {
        uint64_t count = 0;
        double sum = 0.0, sum_sq = 0.0;
        st = pgaccel_reduce_stats_f32(fdata, N, &count, &sum, &sum_sq);
        std::printf("stats_f32 N=%zu: status=%d count=%llu sum=%.4f sum_sq=%.4f\n",
                    N, st, static_cast<unsigned long long>(count), sum, sum_sq);
        if (st != PGACCEL_OK) {
            std::fprintf(stderr, "FAIL stats_f32 status=%d\n", st);
            failures++;
        } else {
            if (count != EXPECTED_COUNT) {
                std::fprintf(stderr,
                    "FAIL stats_f32 count: got %llu expected %llu\n",
                    static_cast<unsigned long long>(count),
                    static_cast<unsigned long long>(EXPECTED_COUNT));
                failures++;
            }
            failures += check_close("stats_f32 sum", sum, EXPECTED_SUM, 1e-3);
            failures += check_close("stats_f32 sum_sq", sum_sq,
                                    EXPECTED_SUM_SQ, 1e-3);
        }
    }

    // -- sum_sq f64 (skip if device lacks fp64) --
    {
        double sum_sq = -1.0;
        st = pgaccel_reduce_sum_sq_f64(ddata, N, &sum_sq);
        std::printf("sum_sq_f64 N=%zu: status=%d sum_sq=%.4f\n", N, st, sum_sq);
        if (st == PGACCEL_UNSUPPORTED) {
            std::printf("  (fp64 not supported — skipping)\n");
        } else if (st != PGACCEL_OK) {
            std::fprintf(stderr, "FAIL sum_sq_f64 status=%d\n", st);
            failures++;
        } else {
            failures += check_close("sum_sq_f64", sum_sq, EXPECTED_SUM_SQ, 1e-6);
        }
    }

    // -- stats f64 (skip if device lacks fp64) --
    {
        uint64_t count = 0;
        double sum = 0.0, sum_sq = 0.0;
        st = pgaccel_reduce_stats_f64(ddata, N, &count, &sum, &sum_sq);
        std::printf("stats_f64 N=%zu: status=%d count=%llu sum=%.4f sum_sq=%.4f\n",
                    N, st, static_cast<unsigned long long>(count), sum, sum_sq);
        if (st == PGACCEL_UNSUPPORTED) {
            std::printf("  (fp64 not supported — skipping)\n");
        } else if (st != PGACCEL_OK) {
            std::fprintf(stderr, "FAIL stats_f64 status=%d\n", st);
            failures++;
        } else {
            if (count != EXPECTED_COUNT) {
                std::fprintf(stderr,
                    "FAIL stats_f64 count: got %llu expected %llu\n",
                    static_cast<unsigned long long>(count),
                    static_cast<unsigned long long>(EXPECTED_COUNT));
                failures++;
            }
            failures += check_close("stats_f64 sum", sum, EXPECTED_SUM, 1e-6);
            failures += check_close("stats_f64 sum_sq", sum_sq,
                                    EXPECTED_SUM_SQ, 1e-6);
        }
    }

    // -- empty input --
    {
        uint64_t count = 99;
        double sum = -1.0, sum_sq = -1.0;
        st = pgaccel_reduce_stats_f32(nullptr, 0, &count, &sum, &sum_sq);
        std::printf("stats_f32 N=0: status=%d count=%llu sum=%.4f sum_sq=%.4f\n",
                    st, static_cast<unsigned long long>(count), sum, sum_sq);
        if (st != PGACCEL_OK || count != 0 || sum != 0.0 || sum_sq != 0.0) {
            std::fprintf(stderr, "FAIL stats_f32 N=0 zero-init contract\n");
            failures++;
        }
    }

    pgaccel_shutdown();

    if (failures > 0) {
        std::fprintf(stderr, "FAIL: %d failure(s)\n", failures);
        return 1;
    }
    std::printf("PASS\n");
    return 0;
}

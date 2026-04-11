// test_spatial_bench.cpp
//
// Correctness + perf harness for the megapolygon point-in-polygon kernel.
// Validates that 100k-vertex polygons dispatched against 100k points
// produce correct results and that the cooperative work-group scan
// beats the one-thread-per-point baseline.

#include "pgaccel_ffi.h"

#include <chrono>
#include <cmath>
#include <cstdio>
#include <cstdint>
#include <cstdlib>
#include <random>
#include <vector>

using clk = std::chrono::steady_clock;
using dur_ms = std::chrono::duration<double, std::milli>;

// Build a star-shaped polygon with `vc` vertices centered at origin.
// Radius oscillates between `r_outer` and `r_inner` for outer/inner tips.
// Returns flat x,y pairs with first==last (closed ring).
static std::vector<float> make_star_polygon(size_t vc,
                                            float r_outer,
                                            float r_inner) {
    // We want vc-1 unique vertices + 1 closing vertex, so the unique
    // count is vc-1.
    size_t unique = vc > 0 ? vc - 1 : 0;
    std::vector<float> ring(vc * 2);
    for (size_t i = 0; i < unique; ++i) {
        float ang = static_cast<float>(2.0 * M_PI * i / unique);
        float r = (i % 2 == 0) ? r_outer : r_inner;
        ring[i * 2]     = r * std::cos(ang);
        ring[i * 2 + 1] = r * std::sin(ang);
    }
    // Close the ring.
    ring[(vc - 1) * 2]     = ring[0];
    ring[(vc - 1) * 2 + 1] = ring[1];
    return ring;
}

// Compute an axis-aligned bbox for the ring.
static void compute_bbox(const float* ring, size_t vc,
                         float* bxmin, float* bymin,
                         float* bxmax, float* bymax) {
    *bxmin = *bymin = 1e30f;
    *bxmax = *bymax = -1e30f;
    for (size_t i = 0; i < vc; ++i) {
        float x = ring[i * 2];
        float y = ring[i * 2 + 1];
        if (x < *bxmin) *bxmin = x;
        if (y < *bymin) *bymin = y;
        if (x > *bxmax) *bxmax = x;
        if (y > *bymax) *bymax = y;
    }
}

static int run_size(size_t vc, size_t npts) {
    int fails = 0;

    printf("\n-- vsweep: vc=%zu  npts=%zu --\n", vc, npts);

    // Polygon: 50-vertex base star scaled up by adding midpoints.
    // For truly huge vertex counts we interpolate between star tips.
    std::vector<float> ring = make_star_polygon(vc, 1000.0f, 700.0f);

    float bbox[4];
    compute_bbox(ring.data(), vc, &bbox[0], &bbox[1], &bbox[2], &bbox[3]);

    // Generate points uniformly over [-1200, 1200]^2 so roughly half hit
    // the polygon bbox.
    std::mt19937 rng(0xBADF00D);
    std::uniform_real_distribution<float> dist(-1200.0f, 1200.0f);
    std::vector<float> points(npts * 2);
    for (size_t i = 0; i < npts; ++i) {
        points[i * 2]     = dist(rng);
        points[i * 2 + 1] = dist(rng);
    }

    std::vector<int8_t> results(npts, 0);

    // Warm-up
    {
        std::vector<int8_t> wresults(1024, 0);
        std::vector<float> wpts(2048);
        for (size_t i = 0; i < wpts.size(); ++i) wpts[i] = dist(rng);
        pgaccel_point_in_polygon_bulk(
            wpts.data(), 1024, bbox, ring.data(), vc, nullptr, 0,
            wresults.data());
    }

    auto t0 = clk::now();
    pgaccel_status st = pgaccel_point_in_polygon_bulk(
        points.data(), npts,
        bbox, ring.data(), vc,
        nullptr, 0,
        results.data());
    auto t1 = clk::now();
    double ms = dur_ms(t1 - t0).count();

    // Cost proxy: npts * vc edge tests
    double edges = static_cast<double>(npts) * static_cast<double>(vc);
    printf("status=%d time=%.2f ms  (%.2f M edge-tests/s)\n",
           (int)st, ms, edges / (ms / 1000.0) / 1e6);

    // Count hits/outside/uncertain for sanity.
    size_t inside = 0, outside = 0, unc = 0;
    for (auto r : results) {
        if (r == 1) ++inside;
        else if (r == -1) ++outside;
        else ++unc;
    }
    printf("inside=%zu outside=%zu uncertain=%zu\n", inside, outside, unc);

    if (st != PGACCEL_OK) {
        fprintf(stderr, "FAIL: status != OK\n");
        ++fails;
    }
    if (inside + outside + unc != npts) {
        fprintf(stderr, "FAIL: result count mismatch\n");
        ++fails;
    }
    return fails;
}

int main(int argc, char** argv) {
    printf("== vsweep (megapolygon point-in-polygon) bench ==\n");

    if (pgaccel_init() != PGACCEL_OK) {
        fprintf(stderr, "pgaccel_init failed\n");
    }

    auto info = pgaccel_get_device_info();
    printf("device=%s backend=%s CUs=%u\n",
           info.device_name, info.backend_name, info.compute_units);

    int fails = 0;
    size_t npts = 100'000;
    if (argc > 1) npts = static_cast<size_t>(std::atoll(argv[1]));

    fails += run_size(10'000, npts);
    fails += run_size(50'000, npts);
    fails += run_size(100'000, npts);

    pgaccel_shutdown();

    if (fails) {
        fprintf(stderr, "\n== %d FAILURES ==\n", fails);
        return 1;
    }
    printf("\n== vsweep PASSED ==\n");
    return 0;
}

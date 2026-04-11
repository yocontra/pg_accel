// test_raster_bench.cpp
//
// Per-row raster kernel benchmark for small (8x8 ... 64x64) tiles.
// Exercises pgaccel_map_algebra and pgaccel_raster_clip the way the
// PG executor does — calling them once per row for many rows.

#include "pgaccel_ffi.h"

#include <chrono>
#include <cmath>
#include <cstdint>
#include <cstdio>
#include <cstdlib>
#include <cstring>
#include <random>
#include <vector>

using clk = std::chrono::steady_clock;
using dur_ms = std::chrono::duration<double, std::milli>;

static void bench_map_algebra(size_t tile, size_t rows) {
    const size_t pc = tile * tile;

    // Expression: band[0] * 2 + 1
    pgaccel_expr_inst code[5];
    code[0].op = PGACCEL_OP_LOAD_BAND; code[0].arg.band_index = 0;
    code[1].op = PGACCEL_OP_LOAD_CONST; code[1].arg.constant = 2.0;
    code[2].op = PGACCEL_OP_MUL;
    code[3].op = PGACCEL_OP_LOAD_CONST; code[3].arg.constant = 1.0;
    code[4].op = PGACCEL_OP_ADD;

    pgaccel_expr expr;
    expr.instructions = code;
    expr.inst_count = 5;
    expr.band_count = 1;

    std::vector<float> band0(pc);
    for (size_t i = 0; i < pc; ++i) band0[i] = static_cast<float>(i);
    std::vector<float> out(pc);
    std::vector<uint8_t> mask(pc, 0);

    // Warm-up
    for (int i = 0; i < 4; ++i) {
        const void* bands[] = { band0.data() };
        pgaccel_map_algebra(bands, pc, PGACCEL_PT_FLOAT32, &expr,
                             out.data(), mask.data());
    }

    auto t0 = clk::now();
    for (size_t r = 0; r < rows; ++r) {
        const void* bands[] = { band0.data() };
        pgaccel_map_algebra(bands, pc, PGACCEL_PT_FLOAT32, &expr,
                             out.data(), mask.data());
    }
    auto t1 = clk::now();
    double ms = dur_ms(t1 - t0).count();

    size_t total_px = pc * rows;
    printf("map_algebra tile=%zux%zu rows=%zu  %.2f ms  "
           "(%.1f us/row, %.1f M px/s)\n",
           tile, tile, rows, ms,
           ms * 1000.0 / rows,
           total_px / (ms / 1000.0) / 1e6);
}

static void bench_raster_clip(size_t tile, size_t rows) {
    // Small triangular clip ring inside the tile.
    float ring[] = {
        0.0f,                       0.0f,
        static_cast<float>(tile),   0.0f,
        static_cast<float>(tile/2), static_cast<float>(tile),
        0.0f,                       0.0f,
    };

    std::vector<float> pixels(tile * tile);
    for (size_t i = 0; i < pixels.size(); ++i)
        pixels[i] = static_cast<float>(i);
    std::vector<float> out(tile * tile);
    std::vector<uint8_t> mask(tile * tile, 0);

    // Warm-up
    for (int i = 0; i < 4; ++i) {
        pgaccel_raster_clip(
            pixels.data(), tile, tile,
            0.0, 0.0, 1.0, 1.0,
            PGACCEL_PT_FLOAT32,
            ring, 4,
            out.data(), mask.data());
    }

    auto t0 = clk::now();
    for (size_t r = 0; r < rows; ++r) {
        pgaccel_raster_clip(
            pixels.data(), tile, tile,
            0.0, 0.0, 1.0, 1.0,
            PGACCEL_PT_FLOAT32,
            ring, 4,
            out.data(), mask.data());
    }
    auto t1 = clk::now();
    double ms = dur_ms(t1 - t0).count();

    printf("raster_clip tile=%zux%zu rows=%zu  %.2f ms  (%.1f us/row)\n",
           tile, tile, rows, ms, ms * 1000.0 / rows);
}

int main(int argc, char** argv) {
    size_t rows = 100'000;
    if (argc > 1) rows = static_cast<size_t>(std::atoll(argv[1]));

    printf("== raster per-row bench (rows=%zu) ==\n", rows);
    if (pgaccel_init() != PGACCEL_OK) {
        fprintf(stderr, "pgaccel_init failed\n");
    }
    auto info = pgaccel_get_device_info();
    printf("device=%s backend=%s CUs=%u\n",
           info.device_name, info.backend_name, info.compute_units);

    printf("\n-- map_algebra --\n");
    bench_map_algebra(8, rows);
    bench_map_algebra(16, rows);
    bench_map_algebra(32, rows);
    bench_map_algebra(64, rows);

    printf("\n-- raster_clip --\n");
    bench_raster_clip(8, rows);
    bench_raster_clip(16, rows);
    bench_raster_clip(32, rows);
    bench_raster_clip(64, rows);

    pgaccel_shutdown();
    printf("\n== raster bench done ==\n");
    return 0;
}

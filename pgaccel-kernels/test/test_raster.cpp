#include <cmath>
#include <cstdio>
#include <cstdlib>
#include <cstring>
#include <vector>

#include "pgaccel_ffi.h"

static int g_tests_passed = 0;
static int g_tests_failed = 0;

#define ASSERT_EQ(a, b, msg)                                                               \
  do {                                                                                     \
    if ((a) != (b)) {                                                                      \
      fprintf(stderr, "FAIL [%s:%d] %s: expected %d, got %d\n", __FILE__, __LINE__, (msg), \
              (int)(b), (int)(a));                                                         \
      g_tests_failed++;                                                                    \
      return;                                                                              \
    }                                                                                      \
  } while (0)

#define ASSERT_NEAR(a, b, eps, msg)                                                                \
  do {                                                                                             \
    double _a = (a), _b = (b);                                                                     \
    if (std::fabs(_a - _b) > (eps)) {                                                              \
      fprintf(stderr, "FAIL [%s:%d] %s: expected %.6f, got %.6f\n", __FILE__, __LINE__, (msg), _b, \
              _a);                                                                                 \
      g_tests_failed++;                                                                            \
      return;                                                                                      \
    }                                                                                              \
  } while (0)

#define PASS(msg)                  \
  do {                             \
    printf("  PASS: %s\n", (msg)); \
    g_tests_passed++;              \
  } while (0)

/* ── Helper: build expression instruction ─────────────────────── */

static pgaccel_expr_inst make_load_band(int idx) {
  pgaccel_expr_inst inst;
  inst.op = PGACCEL_OP_LOAD_BAND;
  inst.arg.band_index = idx;
  return inst;
}

static pgaccel_expr_inst make_load_const(double val) {
  pgaccel_expr_inst inst;
  inst.op = PGACCEL_OP_LOAD_CONST;
  inst.arg.constant = val;
  return inst;
}

static pgaccel_expr_inst make_op(pgaccel_op op) {
  pgaccel_expr_inst inst;
  inst.op = op;
  inst.arg.constant = 0.0;
  return inst;
}

/* ── Test: map algebra [band0] * 2 + 1 on 64x64 float32 ──────── */

static void test_map_algebra_simple() {
  const size_t N = 64 * 64;
  std::vector<float> band0(N);
  for (size_t i = 0; i < N; i++) {
    band0[i] = static_cast<float>(i);
  }

  const void* bands[] = {band0.data()};

  // Expression: band[0] * 2 + 1
  pgaccel_expr_inst code[] = {
      make_load_band(0),    make_load_const(2.0),    make_op(PGACCEL_OP_MUL),
      make_load_const(1.0), make_op(PGACCEL_OP_ADD),
  };
  pgaccel_expr expr;
  expr.instructions = code;
  expr.inst_count = 5;
  expr.band_count = 1;

  std::vector<float> output(N, 0.0f);
  std::vector<uint8_t> nodata(N, 0);

  pgaccel_status st =
      pgaccel_map_algebra(bands, N, PGACCEL_PT_FLOAT32, &expr, output.data(), nodata.data());
  ASSERT_EQ(st, PGACCEL_OK, "map_algebra simple status");

  for (size_t i = 0; i < N; i++) {
    float expected = static_cast<float>(i) * 2.0f + 1.0f;
    ASSERT_NEAR(output[i], expected, 0.01, "map_algebra simple pixel value");
  }

  PASS("map_algebra: [band0] * 2 + 1 on 64x64 float32");
}

/* ── Test: map algebra sqrt(band0^2 + band1^2) two-band ───────── */

static void test_map_algebra_two_band() {
  const size_t N = 256;
  std::vector<float> band0(N), band1(N);
  for (size_t i = 0; i < N; i++) {
    band0[i] = static_cast<float>(i);
    band1[i] = static_cast<float>(N - i);
  }

  const void* bands[] = {band0.data(), band1.data()};

  // Expression: sqrt(band[0]^2 + band[1]^2)
  // Stack: band0, band0, MUL -> band0^2
  //        band1, band1, MUL -> band1^2
  //        ADD -> band0^2 + band1^2
  //        SQRT
  pgaccel_expr_inst code[] = {
      make_load_band(0), make_load_band(0),       make_op(PGACCEL_OP_MUL), make_load_band(1),
      make_load_band(1), make_op(PGACCEL_OP_MUL), make_op(PGACCEL_OP_ADD), make_op(PGACCEL_OP_SQRT),
  };
  pgaccel_expr expr;
  expr.instructions = code;
  expr.inst_count = 8;
  expr.band_count = 2;

  std::vector<float> output(N, 0.0f);
  std::vector<uint8_t> nodata(N, 0);

  pgaccel_status st =
      pgaccel_map_algebra(bands, N, PGACCEL_PT_FLOAT32, &expr, output.data(), nodata.data());
  ASSERT_EQ(st, PGACCEL_OK, "map_algebra two_band status");

  for (size_t i = 0; i < N; i++) {
    double a = static_cast<double>(band0[i]);
    double b = static_cast<double>(band1[i]);
    float expected = static_cast<float>(std::sqrt(a * a + b * b));
    ASSERT_NEAR(output[i], expected, 0.5, "map_algebra two_band pixel value");
  }

  PASS("map_algebra: sqrt(band0^2 + band1^2) two-band");
}

/* ── Test: map algebra with int32 pixel type ──────────────────── */
//
// As of the 2026-05-02 cheat audit, only FP32 pixels are accelerated.
// The previous "int32 support" was a host-side bytecode interpreter
// loop that called pgaccel_record_gpu_exec() while computing on CPU
// (CLAUDE.md rule 11/12 violation). Int32 inputs now return
// PGACCEL_ERROR_UNSUPPORTED so the caller routes through PG via the
// standard unsupported-input path.
static void test_map_algebra_int32() {
  const size_t N = 100;
  std::vector<int32_t> band0(N);
  for (size_t i = 0; i < N; i++) {
    band0[i] = static_cast<int32_t>(i * 10);
  }

  const void* bands[] = {band0.data()};

  // Expression: band[0] + 5
  pgaccel_expr_inst code[] = {
      make_load_band(0),
      make_load_const(5.0),
      make_op(PGACCEL_OP_ADD),
  };
  pgaccel_expr expr;
  expr.instructions = code;
  expr.inst_count = 3;
  expr.band_count = 1;

  std::vector<int32_t> output(N, 0);
  std::vector<uint8_t> nodata(N, 0);

  pgaccel_status st =
      pgaccel_map_algebra(bands, N, PGACCEL_PT_INT32, &expr, output.data(), nodata.data());
  ASSERT_EQ(st, PGACCEL_ERROR_UNSUPPORTED, "map_algebra int32 returns UNSUPPORTED");

  PASS("map_algebra: int32 declined (FP32-only kernel)");
}

/* ── Test: NODATA pixels are preserved ────────────────────────── */

static void test_map_algebra_nodata() {
  const size_t N = 16;
  std::vector<float> band0(N, 42.0f);
  const void* bands[] = {band0.data()};

  // Simple: band[0] * 3
  pgaccel_expr_inst code[] = {
      make_load_band(0),
      make_load_const(3.0),
      make_op(PGACCEL_OP_MUL),
  };
  pgaccel_expr expr;
  expr.instructions = code;
  expr.inst_count = 3;
  expr.band_count = 1;

  std::vector<float> output(N, -1.0f);
  std::vector<uint8_t> nodata(N, 0);

  // Mark some pixels as NODATA
  nodata[2] = 1;
  nodata[5] = 1;
  nodata[10] = 1;

  pgaccel_status st =
      pgaccel_map_algebra(bands, N, PGACCEL_PT_FLOAT32, &expr, output.data(), nodata.data());
  ASSERT_EQ(st, PGACCEL_OK, "map_algebra nodata status");

  // NODATA pixels should remain marked and get zero output
  ASSERT_EQ(nodata[2], 1, "nodata[2] preserved");
  ASSERT_EQ(nodata[5], 1, "nodata[5] preserved");
  ASSERT_EQ(nodata[10], 1, "nodata[10] preserved");
  ASSERT_NEAR(output[2], 0.0, 0.01, "nodata pixel output is zero");

  // Non-NODATA pixels should be computed
  ASSERT_NEAR(output[0], 126.0, 0.01, "non-nodata pixel computed");
  ASSERT_NEAR(output[1], 126.0, 0.01, "non-nodata pixel computed");
  ASSERT_EQ(nodata[0], 0, "non-nodata mask clear");

  PASS("map_algebra: NODATA pixels preserved");
}

/* ── Test: division by zero -> NaN -> NODATA ──────────────────── */

static void test_map_algebra_div_zero() {
  const size_t N = 4;
  std::vector<float> band0 = {10.0f, 20.0f, 30.0f, 40.0f};
  std::vector<float> band1 = {2.0f, 0.0f, 5.0f, 0.0f};
  const void* bands[] = {band0.data(), band1.data()};

  // Expression: band[0] / band[1]
  pgaccel_expr_inst code[] = {
      make_load_band(0),
      make_load_band(1),
      make_op(PGACCEL_OP_DIV),
  };
  pgaccel_expr expr;
  expr.instructions = code;
  expr.inst_count = 3;
  expr.band_count = 2;

  std::vector<float> output(N, -1.0f);
  std::vector<uint8_t> nodata(N, 0);

  pgaccel_status st =
      pgaccel_map_algebra(bands, N, PGACCEL_PT_FLOAT32, &expr, output.data(), nodata.data());
  ASSERT_EQ(st, PGACCEL_OK, "map_algebra div_zero status");

  ASSERT_NEAR(output[0], 5.0, 0.01, "10/2 = 5");
  ASSERT_EQ(nodata[0], 0, "10/2 not nodata");
  ASSERT_EQ(nodata[1], 1, "20/0 -> nodata");
  ASSERT_NEAR(output[2], 6.0, 0.01, "30/5 = 6");
  ASSERT_EQ(nodata[3], 1, "40/0 -> nodata");

  PASS("map_algebra: division by zero -> NODATA");
}

/* ── Test: empty input (count=0) ──────────────────────────────── */

static void test_empty_input() {
  const void* bands[] = {nullptr};
  pgaccel_expr_inst code[] = {make_load_band(0)};
  pgaccel_expr expr;
  expr.instructions = code;
  expr.inst_count = 1;
  expr.band_count = 1;

  pgaccel_status st;

  // Map algebra with 0 pixels
  st = pgaccel_map_algebra(bands, 0, PGACCEL_PT_FLOAT32, &expr, nullptr, nullptr);
  // nullptr output is checked before pixel_count, but with count=0 it should be ok
  // Actually the null check will fire. Let's use a dummy.
  float dummy_out = 0;
  uint8_t dummy_nd = 0;
  float dummy_in = 0;
  const void* dummy_bands[] = {&dummy_in};
  st = pgaccel_map_algebra(dummy_bands, 0, PGACCEL_PT_FLOAT32, &expr, &dummy_out, &dummy_nd);
  ASSERT_EQ(st, PGACCEL_OK, "map_algebra empty");

  // Clip with 0 dimensions
  float ring[] = {0, 0, 1, 0, 1, 1, 0, 1};
  st = pgaccel_raster_clip(&dummy_in, 0, 0, 0, 0, 1, 1, PGACCEL_PT_FLOAT32, ring, 4, &dummy_out,
                           &dummy_nd);
  ASSERT_EQ(st, PGACCEL_OK, "clip empty");

  // Reclass with 0 pixels
  pgaccel_reclass_rule rule = {0, 10, 99};
  st = pgaccel_raster_reclass(&dummy_in, 0, PGACCEL_PT_FLOAT32, &rule, 1, PGACCEL_PT_FLOAT32,
                              &dummy_out);
  ASSERT_EQ(st, PGACCEL_OK, "reclass empty");

  PASS("empty input: no crash");
}

/* ── Test: raster clip with diamond polygon ───────────────────── */

static void test_raster_clip() {
  const size_t W = 32, H = 32;
  const size_t N = W * H;

  std::vector<float> input(N);
  for (size_t i = 0; i < N; i++) {
    input[i] = static_cast<float>(i + 1);
  }

  // Diamond polygon centered at (16, 16) with radius 8
  // Vertices: top, right, bottom, left
  float ring[] = {
      16.0f, 8.0f,   // top
      24.0f, 16.0f,  // right
      16.0f, 24.0f,  // bottom
      8.0f,  16.0f,  // left
  };

  std::vector<float> output(N, 0.0f);
  std::vector<uint8_t> nodata(N, 0);

  // Origin at (0,0), scale 1x1
  pgaccel_status st =
      pgaccel_raster_clip(input.data(), W, H, 0.0, 0.0, 1.0, 1.0, PGACCEL_PT_FLOAT32, ring, 4,
                          output.data(), nodata.data());
  ASSERT_EQ(st, PGACCEL_OK, "clip status");

  // Check pixel at center (16, 16) is inside
  size_t center_idx = 16 * W + 16;
  ASSERT_EQ(nodata[center_idx], 0, "center pixel inside diamond");
  ASSERT_NEAR(output[center_idx], input[center_idx], 0.01, "center pixel value preserved");

  // Check corner pixel (0, 0) is outside
  ASSERT_EQ(nodata[0], 1, "corner pixel outside diamond");

  // Check pixel at (31, 31) is outside
  size_t corner_idx = 31 * W + 31;
  ASSERT_EQ(nodata[corner_idx], 1, "far corner outside diamond");

  // Count inside pixels - diamond area should be roughly half of
  // bounding square (16*16/2 = 128 area, but this is approximate)
  int inside_count = 0;
  for (size_t i = 0; i < N; i++) {
    if (nodata[i] == 0)
      inside_count++;
  }
  // Diamond with vertices at 8,16,24,16 has area = 0.5 * 16 * 16 = 128
  // With pixel-center rounding, approximately 112-144 pixels
  if (inside_count < 80 || inside_count > 180) {
    fprintf(stderr, "FAIL: clip inside count %d not in expected range [80, 180]\n", inside_count);
    g_tests_failed++;
    return;
  }

  PASS("raster_clip: diamond polygon on 32x32");
}

/* ── Test: raster reclass with 3 rules ────────────────────────── */

static void test_raster_reclass() {
  const size_t N = 20;
  std::vector<float> input(N);
  for (size_t i = 0; i < N; i++) {
    input[i] = static_cast<float>(i * 5);  // 0, 5, 10, ..., 95
  }

  pgaccel_reclass_rule rules[] = {
      {0.0, 30.0, 1.0},    // [0, 30)  -> 1
      {30.0, 60.0, 2.0},   // [30, 60) -> 2
      {60.0, 100.0, 3.0},  // [60, 100) -> 3
  };

  std::vector<float> output(N, -1.0f);

  pgaccel_status st = pgaccel_raster_reclass(input.data(), N, PGACCEL_PT_FLOAT32, rules, 3,
                                             PGACCEL_PT_FLOAT32, output.data());
  ASSERT_EQ(st, PGACCEL_OK, "reclass status");

  // input[0]=0 -> rule 0 -> 1.0
  ASSERT_NEAR(output[0], 1.0, 0.01, "reclass 0 -> 1");
  // input[3]=15 -> rule 0 -> 1.0
  ASSERT_NEAR(output[3], 1.0, 0.01, "reclass 15 -> 1");
  // input[6]=30 -> rule 1 -> 2.0
  ASSERT_NEAR(output[6], 2.0, 0.01, "reclass 30 -> 2");
  // input[10]=50 -> rule 1 -> 2.0
  ASSERT_NEAR(output[10], 2.0, 0.01, "reclass 50 -> 2");
  // input[12]=60 -> rule 2 -> 3.0
  ASSERT_NEAR(output[12], 3.0, 0.01, "reclass 60 -> 3");
  // input[18]=90 -> rule 2 -> 3.0
  ASSERT_NEAR(output[18], 3.0, 0.01, "reclass 90 -> 3");

  PASS("raster_reclass: 3-rule classification");
}

/* ── Test: reclass with int32 input, float32 output ───────────── */

static void test_raster_reclass_mixed_types() {
  const size_t N = 10;
  std::vector<int32_t> input(N);
  for (size_t i = 0; i < N; i++) {
    input[i] = static_cast<int32_t>(i * 10);
  }

  pgaccel_reclass_rule rules[] = {
      {0.0, 50.0, 100.0},
      {50.0, 100.0, 200.0},
  };

  std::vector<float> output(N, -1.0f);

  pgaccel_status st = pgaccel_raster_reclass(input.data(), N, PGACCEL_PT_INT32, rules, 2,
                                             PGACCEL_PT_FLOAT32, output.data());
  ASSERT_EQ(st, PGACCEL_OK, "reclass mixed types status");

  // input[0]=0 -> 100
  ASSERT_NEAR(output[0], 100.0, 0.01, "reclass int32 0 -> 100");
  // input[4]=40 -> 100
  ASSERT_NEAR(output[4], 100.0, 0.01, "reclass int32 40 -> 100");
  // input[5]=50 -> 200
  ASSERT_NEAR(output[5], 200.0, 0.01, "reclass int32 50 -> 200");
  // input[9]=90 -> 200
  ASSERT_NEAR(output[9], 200.0, 0.01, "reclass int32 90 -> 200");

  PASS("raster_reclass: int32 -> float32 mixed types");
}

/* ── Test: reclass passthrough when no rule matches ───────────── */

static void test_raster_reclass_passthrough() {
  const size_t N = 4;
  std::vector<float> input = {-5.0f, 150.0f, 25.0f, 999.0f};

  pgaccel_reclass_rule rules[] = {
      {0.0, 50.0, 1.0},
  };

  std::vector<float> output(N, -1.0f);

  pgaccel_status st = pgaccel_raster_reclass(input.data(), N, PGACCEL_PT_FLOAT32, rules, 1,
                                             PGACCEL_PT_FLOAT32, output.data());
  ASSERT_EQ(st, PGACCEL_OK, "reclass passthrough status");

  // -5 -> no rule matches -> passthrough
  ASSERT_NEAR(output[0], -5.0, 0.01, "passthrough -5");
  // 150 -> no rule matches -> passthrough
  ASSERT_NEAR(output[1], 150.0, 0.01, "passthrough 150");
  // 25 -> matches rule -> 1.0
  ASSERT_NEAR(output[2], 1.0, 0.01, "matched 25 -> 1");
  // 999 -> no rule matches -> passthrough
  ASSERT_NEAR(output[3], 999.0, 0.01, "passthrough 999");

  PASS("raster_reclass: passthrough when no rule matches");
}

/* ── Test: raster resample 4x4 -> 8x8 bilinear ────────────────── */

static void test_raster_resample_upsample() {
  // 4x4 source: linearly increasing values 0..15
  const size_t SW = 4, SH = 4;
  std::vector<float> src(SW * SH);
  for (size_t i = 0; i < SW * SH; i++) {
    src[i] = static_cast<float>(i);
  }

  const size_t DW = 8, DH = 8;
  std::vector<float> dst(DW * DH, -999.0f);

  pgaccel_status st = pgaccel_raster_resample(src.data(), SW, SH, DW, DH, dst.data());
  ASSERT_EQ(st, PGACCEL_OK, "resample upsample status");

  // No pixel may keep the sentinel — every dst pixel must have been written.
  for (size_t i = 0; i < DW * DH; i++) {
    if (dst[i] == -999.0f) {
      fprintf(stderr, "FAIL: dst pixel %zu untouched\n", i);
      g_tests_failed++;
      return;
    }
  }

  // Output range must lie within [0, 15] (bilinear of values in that range).
  for (size_t i = 0; i < DW * DH; i++) {
    if (dst[i] < 0.0f || dst[i] > 15.0f) {
      fprintf(stderr, "FAIL: dst[%zu] = %f out of [0, 15]\n", i, dst[i]);
      g_tests_failed++;
      return;
    }
  }

  // Corner pixels: due to the (dc + 0.5) / dw - 0.5 alignment, the dst (0,0)
  // pixel maps to src (-0.25, -0.25) which clamps to src (0,0) = 0.0.
  ASSERT_NEAR(dst[0], 0.0, 0.01, "resample top-left pixel");

  PASS("raster_resample: 4x4 -> 8x8 bilinear");
}

static void test_raster_resample_downsample() {
  const size_t SW = 8, SH = 8;
  std::vector<float> src(SW * SH);
  for (size_t i = 0; i < SW * SH; i++) {
    src[i] = static_cast<float>(i);
  }

  const size_t DW = 4, DH = 4;
  std::vector<float> dst(DW * DH, -1.0f);

  pgaccel_status st = pgaccel_raster_resample(src.data(), SW, SH, DW, DH, dst.data());
  ASSERT_EQ(st, PGACCEL_OK, "resample downsample status");

  // Output values stay within the input range [0, 63].
  for (size_t i = 0; i < DW * DH; i++) {
    if (dst[i] < 0.0f || dst[i] > 63.0f) {
      fprintf(stderr, "FAIL: dst[%zu] = %f out of [0, 63]\n", i, dst[i]);
      g_tests_failed++;
      return;
    }
  }

  PASS("raster_resample: 8x8 -> 4x4 bilinear");
}

/* ── Test: raster slope on synthetic ramp ─────────────────────── */

static void test_raster_slope_constant() {
  // Constant raster -> slope must be 0 everywhere.
  const size_t W = 5, H = 5;
  std::vector<float> src(W * H, 100.0f);
  std::vector<float> slope(W * H, -1.0f);

  pgaccel_status st = pgaccel_raster_slope(src.data(), W, H, 1.0, 1.0, slope.data());
  ASSERT_EQ(st, PGACCEL_OK, "slope constant status");

  for (size_t i = 0; i < W * H; i++) {
    ASSERT_NEAR(slope[i], 0.0, 0.001, "constant slope is zero");
  }

  PASS("raster_slope: constant raster has zero slope");
}

static void test_raster_slope_ramp() {
  // East-west ramp: pixel value increases by 1 per column.
  const size_t W = 5, H = 5;
  std::vector<float> src(W * H);
  for (size_t r = 0; r < H; r++) {
    for (size_t c = 0; c < W; c++) {
      src[r * W + c] = static_cast<float>(c);
    }
  }
  std::vector<float> slope(W * H, -1.0f);

  pgaccel_status st = pgaccel_raster_slope(src.data(), W, H, 1.0, 1.0, slope.data());
  ASSERT_EQ(st, PGACCEL_OK, "slope ramp status");

  // Interior pixels: dz/dx = 1 (per cell), dz/dy = 0
  // slope = atan(1) = 45 degrees
  size_t centre = 2 * W + 2;
  ASSERT_NEAR(slope[centre], 45.0, 1.0, "ramp slope is ~45 degrees");

  // Edge pixels are 0 by convention
  ASSERT_NEAR(slope[0], 0.0, 0.001, "edge pixel is 0");
  ASSERT_NEAR(slope[W - 1], 0.0, 0.001, "edge pixel is 0");

  PASS("raster_slope: 1-per-cell east-west ramp -> 45 degree interior");
}

/* ── Test: raster aspect on east-facing ramp ──────────────────── */

static void test_raster_aspect_east_ramp() {
  // East-west ramp: pixel value increases east -> downward slope faces west.
  // (gdaldem aspect: direction OF steepest descent, value increasing east
  //  means descent towards west = 270 deg.)
  const size_t W = 5, H = 5;
  std::vector<float> src(W * H);
  for (size_t r = 0; r < H; r++) {
    for (size_t c = 0; c < W; c++) {
      src[r * W + c] = static_cast<float>(c);
    }
  }
  std::vector<float> aspect(W * H, -999.0f);

  pgaccel_status st = pgaccel_raster_aspect(src.data(), W, H, aspect.data());
  ASSERT_EQ(st, PGACCEL_OK, "aspect east-ramp status");

  // Edge pixels = -1
  ASSERT_NEAR(aspect[0], -1.0, 0.001, "edge pixel = -1");

  // Interior centre pixel: descent direction is West = 270°
  size_t centre = 2 * W + 2;
  ASSERT_NEAR(aspect[centre], 270.0, 1.0, "east-ramp interior aspect = 270");

  PASS("raster_aspect: east-facing ramp interior aspect = 270 degrees");
}

static void test_raster_aspect_flat() {
  // Flat raster -> aspect = -1 everywhere.
  const size_t W = 4, H = 4;
  std::vector<float> src(W * H, 50.0f);
  std::vector<float> aspect(W * H, 0.0f);

  pgaccel_status st = pgaccel_raster_aspect(src.data(), W, H, aspect.data());
  ASSERT_EQ(st, PGACCEL_OK, "aspect flat status");

  for (size_t i = 0; i < W * H; i++) {
    ASSERT_NEAR(aspect[i], -1.0, 0.001, "flat aspect = -1");
  }

  PASS("raster_aspect: flat raster -> aspect -1 everywhere");
}

/* ── Test: raster hillshade smoke ─────────────────────────────── */

static void test_raster_hillshade_basic() {
  // Synthetic ramp; verify output stays within [0, 255].
  const size_t W = 8, H = 8;
  std::vector<float> src(W * H);
  for (size_t r = 0; r < H; r++) {
    for (size_t c = 0; c < W; c++) {
      src[r * W + c] = static_cast<float>(c + r);
    }
  }
  std::vector<float> shade(W * H, -1.0f);

  pgaccel_status st =
      pgaccel_raster_hillshade(src.data(), W, H, 1.0, 1.0, 315.0, 45.0, 1.0, shade.data());
  ASSERT_EQ(st, PGACCEL_OK, "hillshade status");

  // Edge pixels = 0
  ASSERT_NEAR(shade[0], 0.0, 0.001, "edge pixel = 0");

  // Interior pixels in [0, 255]
  for (size_t r = 1; r < H - 1; r++) {
    for (size_t c = 1; c < W - 1; c++) {
      float v = shade[r * W + c];
      if (v < 0.0f || v > 255.001f) {
        fprintf(stderr, "FAIL: shade[%zu, %zu] = %f out of [0, 255]\n", r, c, v);
        g_tests_failed++;
        return;
      }
    }
  }

  PASS("raster_hillshade: 8x8 ramp produces values in [0, 255]");
}

/* ── Test: raster value lookup at known points ────────────────── */

static void test_raster_value_4x4() {
  // 4x4 raster: pixel(col, row) = row * 10 + col
  const size_t W = 4, H = 4;
  std::vector<float> rast(W * H);
  for (size_t r = 0; r < H; r++) {
    for (size_t c = 0; c < W; c++) {
      rast[r * W + c] = static_cast<float>(r * 10 + c);
    }
  }

  // Origin at world (0, 0); scale 1 east, -1 north (PostGIS standard:
  // pixel rows go top->bottom, scale_y is negative).
  // Pixel (col=2, row=1) world coord:
  //   x = 0 + (2 + 0.5) * 1 = 2.5;  y = 0 + (1 + 0.5) * -1 = -1.5
  std::vector<double> points = {
      0.5,  -0.5,  // (col=0, row=0) -> 0
      2.5,  -1.5,  // (col=2, row=1) -> 12
      3.5,  -3.5,  // (col=3, row=3) -> 33
      -1.0, -1.0,  // out of bounds (col<0)
      4.5,  -1.0,  // out of bounds (col>=4)
  };
  std::vector<double> out(points.size() / 2, -999.0);

  pgaccel_status st = pgaccel_raster_value(rast.data(), W, H, 0.0, 0.0, 1.0, -1.0, points.data(),
                                           out.size(), out.data());
  ASSERT_EQ(st, PGACCEL_OK, "value status");

  ASSERT_NEAR(out[0], 0.0, 0.01, "raster_value lookup at (0.5, -0.5) = 0");
  ASSERT_NEAR(out[1], 12.0, 0.01, "raster_value lookup at (2.5, -1.5) = 12");
  ASSERT_NEAR(out[2], 33.0, 0.01, "raster_value lookup at (3.5, -3.5) = 33");
  // Out-of-bounds points should be NaN (NaN != itself).
  if (out[3] == out[3]) {
    fprintf(stderr, "FAIL: out-of-bounds point #3 not NaN: %f\n", out[3]);
    g_tests_failed++;
    return;
  }
  if (out[4] == out[4]) {
    fprintf(stderr, "FAIL: out-of-bounds point #4 not NaN: %f\n", out[4]);
    g_tests_failed++;
    return;
  }

  PASS("raster_value: 4x4 lookup at known cells + OOB -> NaN");
}

/* ── Test: raster summarystats output layout ──────────────────── */

static void test_raster_summarystats_layout() {
  // Two rasters: row 0 = [1, 2, 3, 4]; row 1 = [10, 20, 30, 40]
  const size_t ROWS = 2;
  const size_t PPR = 4;
  std::vector<float> pix(ROWS * PPR);
  pix[0] = 1.0f;
  pix[1] = 2.0f;
  pix[2] = 3.0f;
  pix[3] = 4.0f;
  pix[4] = 10.0f;
  pix[5] = 20.0f;
  pix[6] = 30.0f;
  pix[7] = 40.0f;

  std::vector<double> out(ROWS * 6, -1.0);

  pgaccel_status st =
      pgaccel_raster_summarystats(pix.data(), ROWS, PPR, /*nodata_masks=*/nullptr, out.data());
  ASSERT_EQ(st, PGACCEL_OK, "summarystats status");

  // Row 0: [1,2,3,4] -> count=4, sum=10, mean=2.5, stddev=sqrt(1.25)≈1.118,
  // min=1, max=4
  ASSERT_NEAR(out[0 * 6 + 0], 4.0, 0.001, "row 0 count");
  ASSERT_NEAR(out[0 * 6 + 1], 10.0, 0.001, "row 0 sum");
  ASSERT_NEAR(out[0 * 6 + 2], 2.5, 0.001, "row 0 mean");
  ASSERT_NEAR(out[0 * 6 + 3], 1.118, 0.01, "row 0 stddev (population)");
  ASSERT_NEAR(out[0 * 6 + 4], 1.0, 0.001, "row 0 min");
  ASSERT_NEAR(out[0 * 6 + 5], 4.0, 0.001, "row 0 max");

  // Row 1: [10,20,30,40] -> count=4, sum=100, mean=25, stddev=sqrt(125)≈11.18,
  // min=10, max=40
  ASSERT_NEAR(out[1 * 6 + 0], 4.0, 0.001, "row 1 count");
  ASSERT_NEAR(out[1 * 6 + 1], 100.0, 0.001, "row 1 sum");
  ASSERT_NEAR(out[1 * 6 + 2], 25.0, 0.001, "row 1 mean");
  ASSERT_NEAR(out[1 * 6 + 3], 11.18, 0.1, "row 1 stddev (population)");
  ASSERT_NEAR(out[1 * 6 + 4], 10.0, 0.001, "row 1 min");
  ASSERT_NEAR(out[1 * 6 + 5], 40.0, 0.001, "row 1 max");

  PASS("raster_summarystats: 6-scalar per-row layout (count/sum/mean/stddev/min/max)");
}

static void test_raster_summarystats_with_mask() {
  // 1 raster, 4 pixels, pixel idx 1 is NoData
  const size_t ROWS = 1;
  const size_t PPR = 4;
  std::vector<float> pix = {5.0f, 999.0f, 7.0f, 9.0f};
  std::vector<uint8_t> mask = {0, 1, 0, 0};

  std::vector<double> out(ROWS * 6, -1.0);

  pgaccel_status st = pgaccel_raster_summarystats(pix.data(), ROWS, PPR, mask.data(), out.data());
  ASSERT_EQ(st, PGACCEL_OK, "summarystats with mask status");

  // Effective values [5, 7, 9] -> count=3, sum=21, mean=7, stddev=sqrt(8/3)≈1.633
  ASSERT_NEAR(out[0], 3.0, 0.001, "masked count");
  ASSERT_NEAR(out[1], 21.0, 0.001, "masked sum");
  ASSERT_NEAR(out[2], 7.0, 0.001, "masked mean");
  ASSERT_NEAR(out[3], 1.633, 0.01, "masked stddev (population)");
  ASSERT_NEAR(out[4], 5.0, 0.001, "masked min");
  ASSERT_NEAR(out[5], 9.0, 0.001, "masked max");

  PASS("raster_summarystats: NoData mask correctly excludes pixels");
}

/* ── main ─────────────────────────────────────────────────────── */

int main() {
  printf("=== pgaccel raster kernel tests ===\n\n");

  pgaccel_status init_status = pgaccel_init();
  if (init_status != PGACCEL_OK) {
    fprintf(stderr, "pgaccel_init() failed with status %d\n", init_status);
    return 1;
  }

  test_map_algebra_simple();
  test_map_algebra_two_band();
  test_map_algebra_int32();
  test_map_algebra_nodata();
  test_map_algebra_div_zero();
  test_empty_input();
  test_raster_clip();
  test_raster_reclass();
  test_raster_reclass_mixed_types();
  test_raster_reclass_passthrough();

  // 6 new kernels (Agent 3A).
  test_raster_resample_upsample();
  test_raster_resample_downsample();
  test_raster_slope_constant();
  test_raster_slope_ramp();
  test_raster_aspect_east_ramp();
  test_raster_aspect_flat();
  test_raster_hillshade_basic();
  test_raster_value_4x4();
  test_raster_summarystats_layout();
  test_raster_summarystats_with_mask();

  pgaccel_shutdown();

  printf("\n=== Results: %d passed, %d failed ===\n", g_tests_passed, g_tests_failed);

  return (g_tests_failed > 0) ? 1 : 0;
}

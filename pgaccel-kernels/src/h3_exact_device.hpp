// Exact H3 lat/lng -> cell device implementation derived from upstream H3 C.
// Source: h3lib in h3-pg 4.2.3 build dependency (Apache-2.0, Uber Technologies).
// This file keeps only the latLngToCell call chain and the lookup tables it uses.
// Do not replace this path with an approximate face/base-cell rewrite; h3_bulk
// correctness depends on matching the same H3 Core semantics h3-pg exposes.
#pragma once

#include <cmath>
#include <cstdint>

namespace pgaccel_h3_exact {

using H3Index = uint64_t;

static constexpr int MAX_H3_RES = 15;
static constexpr int NUM_ICOSA_FACES = 20;
static constexpr int NUM_BASE_CELLS = 122;
static constexpr double M_PI_LOCAL = 3.14159265358979323846;
static constexpr double M_PI_2_LOCAL = 1.57079632679489661923;
static constexpr double M_PI_180 = 0.0174532925199432957692369076848861271111;
static constexpr double EPSILON = 0.0000000000000001;
static constexpr double M_RSIN60 = 1.1547005383792515290182975610039149112953;
static constexpr double M_ONESEVENTH = 0.14285714285714285714285714285714285;
static constexpr double INV_RES0_U_GNOMONIC = 2.61803398874989588842;
static constexpr double M_SQRT7 = 2.6457513110645905905016157536392604257102;
static constexpr double RES_SCALE[16] = {
    2.618033988749895,  6.926666858146697,  18.326237921249273, 48.48666800702688,
    128.28366544874493, 339.4066760491882,  897.9856581412146,  2375.846732344318,
    6285.8996069885025, 16630.927126410224, 44001.297248919516, 116416.48988487158,
    308009.0807424367,  814915.4291941011,  2156063.565197057,  5704408.004358709,
};

static constexpr int H3_MODE_OFFSET = 59;
static constexpr int H3_BC_OFFSET = 45;
static constexpr int H3_RES_OFFSET = 52;
static constexpr int H3_RESERVED_OFFSET = 56;
static constexpr int H3_PER_DIGIT_OFFSET = 3;
static constexpr uint64_t H3_MODE_MASK = (uint64_t(15) << H3_MODE_OFFSET);
static constexpr uint64_t H3_MODE_MASK_NEGATIVE = ~H3_MODE_MASK;
static constexpr uint64_t H3_BC_MASK = (uint64_t(127) << H3_BC_OFFSET);
static constexpr uint64_t H3_BC_MASK_NEGATIVE = ~H3_BC_MASK;
static constexpr uint64_t H3_RES_MASK_EXACT = (uint64_t(15) << H3_RES_OFFSET);
static constexpr uint64_t H3_RES_MASK_NEGATIVE = ~H3_RES_MASK_EXACT;
static constexpr uint64_t H3_DIGIT_MASK = uint64_t(7);
static constexpr uint64_t H3_INIT = UINT64_C(35184372088831);
static constexpr int H3_CELL_MODE = 1;

struct LatLng {
  double lat;
  double lng;
};
struct Vec2d {
  double x;
  double y;
};
struct Vec3d {
  double x;
  double y;
  double z;
};
struct CoordIJK {
  int i;
  int j;
  int k;
};
struct FaceIJK {
  int face;
  CoordIJK coord;
};
struct BaseCellData {
  FaceIJK homeFijk;
  int isPentagon;
  int cwOffsetPent[2];
};
struct BaseCellRotation {
  int baseCell;
  int ccwRot60;
};

// H3 digit values from coordijk.h.
enum Direction {
  CENTER_DIGIT = 0,
  K_AXES_DIGIT = 1,
  J_AXES_DIGIT = 2,
  JK_AXES_DIGIT = 3,
  I_AXES_DIGIT = 4,
  IK_AXES_DIGIT = 5,
  IJ_AXES_DIGIT = 6,
  INVALID_DIGIT = 7,
  NUM_DIGITS = 7,
};

static constexpr CoordIJK UNIT_VECS[7] = {
    {0, 0, 0}, {0, 0, 1}, {0, 1, 0}, {0, 1, 1}, {1, 0, 0}, {1, 0, 1}, {1, 1, 0},
};

static inline double sin_core_half_pi(double x) {
  const double z = x * x;
  double p = -8.2206352466243297e-18;
  p = 2.8114572543455208e-15 + z * p;
  p = -7.6471637318198164e-13 + z * p;
  p = 1.6059043836821613e-10 + z * p;
  p = -2.5052108385441720e-8 + z * p;
  p = 2.7557319223985893e-6 + z * p;
  p = -1.9841269841269841e-4 + z * p;
  p = 8.3333333333333332e-3 + z * p;
  p = -1.6666666666666666e-1 + z * p;
  p = 1.0 + z * p;
  return x * p;
}

static inline double cos_core_half_pi(double x) {
  const double z = x * x;
  double p = 4.1103176233121648e-19;
  p = -1.5619206968586225e-16 + z * p;
  p = 4.7794773323873853e-14 + z * p;
  p = -1.1470745597729725e-11 + z * p;
  p = 2.0876756987868098e-9 + z * p;
  p = -2.7557319223985893e-7 + z * p;
  p = 2.4801587301587302e-5 + z * p;
  p = -1.3888888888888889e-3 + z * p;
  p = 4.1666666666666664e-2 + z * p;
  p = -5.0e-1 + z * p;
  p = 1.0 + z * p;
  return p;
}

static inline void sincos_pi_range(double x, double& s, double& c) {
  if (x > M_PI_2_LOCAL) {
    const double y = M_PI_LOCAL - x;
    s = sin_core_half_pi(y);
    c = -cos_core_half_pi(y);
    return;
  }
  if (x < -M_PI_2_LOCAL) {
    const double y = -M_PI_LOCAL - x;
    s = sin_core_half_pi(y);
    c = -cos_core_half_pi(y);
    return;
  }
  s = sin_core_half_pi(x);
  c = cos_core_half_pi(x);
}

static constexpr Vec3d faceCenterPoint[NUM_ICOSA_FACES] = {
    {0.2199307791404606, 0.6583691780274996, 0.7198475378926182},     // face  0
    {-0.2139234834501421, 0.1478171829550703, 0.9656017935214205},    // face  1
    {0.1092625278784797, -0.4811951572873210, 0.8697775121287253},    // face  2
    {0.7428567301586791, -0.3593941678278028, 0.5648005936517033},    // face  3
    {0.8112534709140969, 0.3448953237639384, 0.4721387736413930},     // face  4
    {-0.1055498149613921, 0.9794457296411413, 0.1718874610009365},    // face  5
    {-0.8075407579970092, 0.1533552485898818, 0.5695261994882688},    // face  6
    {-0.2846148069787907, -0.8644080972654206, 0.4144792552473539},   // face  7
    {0.7405621473854482, -0.6673299564565524, -0.0789837646326737},   // face  8
    {0.8512303986474293, 0.4722343788582681, -0.2289137388687808},    // face  9
    {-0.7405621473854481, 0.6673299564565524, 0.0789837646326737},    // face 10
    {-0.8512303986474292, -0.4722343788582682, 0.2289137388687808},   // face 11
    {0.1055498149613919, -0.9794457296411413, -0.1718874610009365},   // face 12
    {0.8075407579970092, -0.1533552485898819, -0.5695261994882688},   // face 13
    {0.2846148069787908, 0.8644080972654204, -0.4144792552473539},    // face 14
    {-0.7428567301586791, 0.3593941678278027, -0.5648005936517033},   // face 15
    {-0.8112534709140971, -0.3448953237639382, -0.4721387736413930},  // face 16
    {-0.2199307791404607, -0.6583691780274996, -0.7198475378926182},  // face 17
    {0.2139234834501420, -0.1478171829550704, -0.9656017935214205},   // face 18
    {-0.1092625278784796, 0.4811951572873210, -0.8697775121287253},   // face 19
};

static constexpr double M_AP7_ROT_COS = 0.9449111825230680;
static constexpr double M_AP7_ROT_SIN = 0.32732683535398854;

static constexpr double faceAxesAzRadsCII0Cos[NUM_ICOSA_FACES] = {
    0.78800956975116765,  0.86640143358656341,  0.71076326304973791,
    0.90876998541936294,  0.98833108517101020,  -0.90100531245607707,
    -0.98744467805721736, -0.92440665373505637, -0.93843919948189869,
    -0.99044096569994255, 0.93843919948189880,  0.99044096569994255,
    0.90100531245607696,  0.98744467805721736,  0.92440665373505626,
    -0.90876998541936294, -0.98833108517101032, -0.78800956975116787,
    -0.86640143358656341, -0.71076326304973780,
};

static constexpr double faceAxesAzRadsCII0Sin[NUM_ICOSA_FACES] = {
    -0.61566299058866591, -0.49934813094588404, 0.70343129294828011,
    0.41729739227664830,  -0.15232093121003865, 0.43380805309021980,
    0.15796521065246710,  -0.38140836190408245, -0.34544445121578243,
    0.13793728090463883,  -0.34544445121578238, 0.13793728090463869,
    0.43380805309021997,  0.15796521065246730,  -0.38140836190408278,
    0.41729739227664842,  -0.15232093121003829, -0.61566299058866558,
    -0.49934813094588409, 0.70343129294828022,
};

static constexpr double faceCenterLatSin[NUM_ICOSA_FACES] = {
    0.71984753789261824,  0.96560179352142050,  0.86977751212872534,
    0.56480059365170332,  0.47213877364139301,  0.17188746100093655,
    0.56952619948826888,  0.41447925524735385,  -0.07898376463267370,
    -0.22891373886878078, 0.07898376463267370,  0.22891373886878078,
    -0.17188746100093655, -0.56952619948826888, -0.41447925524735385,
    -0.56480059365170332, -0.47213877364139301, -0.71984753789261824,
    -0.96560179352142050, -0.86977751212872534,
};

static constexpr double faceCenterLatCos[NUM_ICOSA_FACES] = {
    0.69413220800502806, 0.26002533789655180, 0.49344409956464680,
    0.82522741678320621, 0.88152423586898698, 0.98511659246540539,
    0.82197317966978012, 0.91005876017408838, 0.99687590246953528,
    0.97344671151384332, 0.99687590246953528, 0.97344671151384332,
    0.98511659246540539, 0.82197317966978012, 0.91005876017408838,
    0.82522741678320621, 0.88152423586898698, 0.69413220800502806,
    0.26002533789655180, 0.49344409956464680,
};

static constexpr double faceCenterLngSin[NUM_ICOSA_FACES] = {
    0.94847807151851771,  0.56847222717148338,  -0.97517663644548025,
    -0.43550924329289276, 0.39124882757641738,  0.99424346024862709,
    0.18656965018188412,  -0.94983767542665576, -0.66942129386756455,
    0.48511579860789583,  0.66942129386756455,  -0.48511579860789600,
    -0.99424346024862709, -0.18656965018188418, 0.94983767542665565,
    0.43550924329289270,  -0.39124882757641727, -0.94847807151851771,
    -0.56847222717148360, 0.97517663644548025,
};

static constexpr double faceCenterLngCos[NUM_ICOSA_FACES] = {
    0.31684278096323049,  -0.82270245346339732, 0.22142838059038344,
    0.90018425836405958,  0.92028493137727663,  -0.10714449007222328,
    -0.98244173650706090, -0.31274332980910563, 0.74288298628833582,
    0.87444991963006291,  -0.74288298628833582, -0.87444991963006280,
    0.10714449007222318,  0.98244173650706090,  0.31274332980910574,
    -0.90018425836405958, -0.92028493137727674, -0.31684278096323054,
    0.82270245346339721,  -0.22142838059038333,
};

static constexpr int faceIjkBaseCell[NUM_ICOSA_FACES * 27] = {
    16,  18,  24,  33,  30,  32,  49,  48,  50,  8,   5,   10,  22,  16,  18,  41,  33,  30,  4,
    0,   2,   15,  8,   5,   31,  22,  16,  2,   6,   14,  10,  11,  17,  24,  23,  25,  0,   1,
    9,   5,   2,   6,   18,  10,  11,  4,   3,   7,   8,   0,   1,   16,  5,   2,   7,   21,  38,
    9,   19,  34,  14,  20,  36,  3,   13,  29,  1,   7,   21,  6,   9,   19,  4,   12,  26,  0,
    3,   13,  2,   1,   7,   26,  42,  58,  29,  43,  62,  38,  47,  64,  12,  28,  44,  13,  26,
    42,  21,  29,  43,  4,   15,  31,  3,   12,  28,  7,   13,  26,  31,  41,  49,  44,  53,  61,
    58,  65,  75,  15,  22,  33,  28,  31,  41,  42,  44,  53,  4,   8,   16,  12,  15,  22,  26,
    28,  31,  50,  48,  49,  32,  30,  33,  24,  18,  16,  70,  67,  66,  52,  50,  48,  37,  32,
    30,  83,  87,  85,  74,  70,  67,  57,  52,  50,  25,  23,  24,  17,  11,  10,  14,  6,   2,
    45,  39,  37,  35,  25,  23,  27,  17,  11,  63,  59,  57,  56,  45,  39,  46,  35,  25,  36,
    20,  14,  34,  19,  9,   38,  21,  7,   55,  40,  27,  54,  36,  20,  51,  34,  19,  72,  60,
    46,  73,  55,  40,  71,  54,  36,  64,  47,  38,  62,  43,  29,  58,  42,  26,  84,  69,  51,
    82,  64,  47,  76,  62,  43,  97,  89,  71,  98,  84,  69,  96,  82,  64,  75,  65,  58,  61,
    53,  44,  49,  41,  31,  94,  86,  76,  81,  75,  65,  66,  61,  53,  107, 104, 96,  101, 94,
    86,  85,  81,  75,  57,  59,  63,  74,  78,  79,  83,  92,  95,  37,  39,  45,  52,  57,  59,
    70,  74,  78,  24,  23,  25,  32,  37,  39,  50,  52,  57,  46,  60,  72,  56,  68,  80,  63,
    77,  90,  27,  40,  55,  35,  46,  60,  45,  56,  68,  14,  20,  36,  17,  27,  40,  25,  35,
    46,  71,  89,  97,  73,  91,  103, 72,  88,  105, 51,  69,  84,  54,  71,  89,  55,  73,  91,
    38,  47,  64,  34,  51,  69,  36,  54,  71,  96,  104, 107, 98,  110, 115, 97,  111, 119, 76,
    86,  94,  82,  96,  104, 84,  98,  110, 58,  65,  75,  62,  76,  86,  64,  82,  96,  85,  87,
    83,  101, 102, 100, 107, 112, 114, 66,  67,  70,  81,  85,  87,  94,  101, 102, 49,  48,  50,
    61,  66,  67,  75,  81,  85,  95,  92,  83,  79,  78,  74,  63,  59,  57,  109, 108, 100, 93,
    95,  92,  77,  79,  78,  117, 118, 114, 106, 109, 108, 90,  93,  95,  90,  77,  63,  80,  68,
    56,  72,  60,  46,  106, 93,  79,  99,  90,  77,  88,  80,  68,  117, 109, 95,  113, 106, 93,
    105, 99,  90,  105, 88,  72,  103, 91,  73,  97,  89,  71,  113, 99,  80,  116, 105, 88,  111,
    103, 91,  117, 106, 90,  121, 113, 99,  119, 116, 105, 119, 111, 97,  115, 110, 98,  107, 104,
    96,  121, 116, 103, 120, 119, 111, 112, 115, 110, 117, 113, 105, 118, 121, 116, 114, 120, 119,
    114, 112, 107, 100, 102, 101, 83,  87,  85,  118, 120, 115, 108, 114, 112, 92,  100, 102, 117,
    121, 119, 109, 118, 120, 95,  108, 114,
};

static constexpr int faceIjkBaseCellRot[NUM_ICOSA_FACES * 27] = {
    0, 0, 0, 0, 0, 3, 1, 3, 3, 0, 5, 5, 0, 0, 0, 1, 0, 0, 0, 5, 5, 1, 0, 5, 1, 0, 0, 0, 0, 0, 0, 0,
    3, 1, 3, 3, 0, 5, 5, 0, 0, 0, 1, 0, 0, 1, 5, 5, 1, 0, 5, 1, 0, 0, 0, 0, 0, 0, 0, 3, 1, 3, 3, 0,
    5, 5, 0, 0, 0, 1, 0, 0, 2, 5, 5, 1, 0, 5, 1, 0, 0, 0, 0, 0, 0, 0, 3, 1, 3, 3, 0, 5, 5, 0, 0, 0,
    1, 0, 0, 3, 5, 5, 1, 0, 5, 1, 0, 0, 0, 0, 0, 0, 0, 3, 1, 3, 3, 0, 5, 5, 0, 0, 0, 1, 0, 0, 4, 5,
    5, 1, 0, 5, 1, 0, 0, 0, 0, 3, 0, 3, 3, 3, 3, 3, 0, 0, 3, 3, 0, 0, 3, 0, 3, 0, 3, 3, 3, 0, 0, 1,
    3, 0, 0, 0, 3, 0, 3, 3, 3, 3, 3, 0, 0, 3, 3, 0, 0, 3, 0, 3, 0, 3, 3, 3, 0, 0, 3, 3, 0, 0, 0, 3,
    0, 3, 3, 3, 3, 3, 0, 0, 3, 3, 0, 0, 3, 0, 3, 0, 3, 3, 3, 0, 0, 3, 3, 0, 0, 0, 3, 0, 3, 3, 3, 3,
    3, 0, 0, 3, 3, 0, 0, 3, 0, 3, 0, 3, 3, 3, 0, 0, 3, 3, 0, 0, 0, 3, 0, 3, 3, 3, 3, 3, 0, 0, 3, 3,
    0, 0, 3, 0, 3, 0, 3, 3, 3, 0, 0, 3, 3, 0, 0, 0, 3, 0, 3, 3, 3, 3, 3, 0, 3, 3, 0, 0, 0, 3, 0, 3,
    0, 3, 3, 3, 0, 3, 3, 0, 0, 0, 0, 3, 0, 3, 3, 3, 3, 3, 0, 3, 3, 0, 0, 0, 3, 0, 3, 0, 3, 3, 3, 0,
    3, 3, 0, 0, 0, 0, 3, 0, 3, 3, 3, 3, 3, 0, 3, 3, 0, 0, 0, 3, 0, 3, 0, 3, 3, 3, 0, 3, 3, 0, 0, 0,
    0, 3, 0, 3, 3, 3, 3, 3, 0, 3, 3, 0, 0, 0, 3, 0, 3, 0, 3, 3, 3, 0, 3, 3, 0, 0, 0, 0, 3, 0, 3, 3,
    3, 3, 3, 0, 3, 3, 0, 0, 0, 3, 0, 3, 0, 3, 3, 3, 0, 3, 3, 0, 0, 0, 0, 0, 0, 0, 3, 1, 3, 3, 0, 0,
    5, 1, 0, 0, 1, 0, 0, 4, 5, 5, 1, 0, 0, 1, 1, 0, 0, 0, 0, 0, 0, 3, 1, 3, 3, 0, 0, 5, 1, 0, 0, 1,
    0, 0, 3, 5, 5, 1, 0, 0, 1, 1, 0, 0, 0, 0, 0, 0, 3, 1, 3, 3, 0, 0, 5, 1, 0, 0, 1, 0, 0, 2, 5, 5,
    1, 0, 0, 1, 1, 0, 0, 0, 0, 0, 0, 3, 1, 3, 3, 0, 0, 5, 1, 0, 0, 1, 0, 0, 1, 5, 5, 1, 0, 0, 1, 1,
    0, 0, 0, 0, 0, 0, 3, 1, 3, 3, 0, 0, 5, 1, 0, 0, 1, 0, 0, 0, 5, 5, 1, 0, 0, 1, 1, 0,
};

static constexpr int baseCellIsPentagon[NUM_BASE_CELLS] = {
    0, 0, 0, 0, 1, 0, 0, 0, 0, 0, 0, 0, 0, 0, 1, 0, 0, 0, 0, 0, 0, 0, 0, 0, 1, 0, 0, 0, 0, 0, 0,
    0, 0, 0, 0, 0, 0, 0, 1, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 1, 0, 0, 0, 0, 0, 0, 0, 0, 1, 0, 0, 0,
    0, 1, 0, 0, 0, 0, 0, 0, 0, 0, 1, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 1, 0, 0, 0, 0, 0, 0, 0, 0, 0,
    0, 0, 0, 0, 1, 0, 0, 0, 0, 0, 0, 0, 0, 0, 1, 0, 0, 0, 0, 0, 0, 0, 0, 0, 1, 0, 0, 0, 0,
};

static constexpr int baseCellCwOffsetPent[NUM_BASE_CELLS][2] = {
    {0, 0}, {0, 0}, {0, 0},   {0, 0},   {-1, -1}, {0, 0}, {0, 0}, {0, 0},   {0, 0}, {0, 0},
    {0, 0}, {0, 0}, {0, 0},   {0, 0},   {2, 6},   {0, 0}, {0, 0}, {0, 0},   {0, 0}, {0, 0},
    {0, 0}, {0, 0}, {0, 0},   {0, 0},   {1, 5},   {0, 0}, {0, 0}, {0, 0},   {0, 0}, {0, 0},
    {0, 0}, {0, 0}, {0, 0},   {0, 0},   {0, 0},   {0, 0}, {0, 0}, {0, 0},   {3, 7}, {0, 0},
    {0, 0}, {0, 0}, {0, 0},   {0, 0},   {0, 0},   {0, 0}, {0, 0}, {0, 0},   {0, 0}, {0, 9},
    {0, 0}, {0, 0}, {0, 0},   {0, 0},   {0, 0},   {0, 0}, {0, 0}, {0, 0},   {4, 8}, {0, 0},
    {0, 0}, {0, 0}, {0, 0},   {11, 15}, {0, 0},   {0, 0}, {0, 0}, {0, 0},   {0, 0}, {0, 0},
    {0, 0}, {0, 0}, {12, 16}, {0, 0},   {0, 0},   {0, 0}, {0, 0}, {0, 0},   {0, 0}, {0, 0},
    {0, 0}, {0, 0}, {0, 0},   {10, 19}, {0, 0},   {0, 0}, {0, 0}, {0, 0},   {0, 0}, {0, 0},
    {0, 0}, {0, 0}, {0, 0},   {0, 0},   {0, 0},   {0, 0}, {0, 0}, {13, 17}, {0, 0}, {0, 0},
    {0, 0}, {0, 0}, {0, 0},   {0, 0},   {0, 0},   {0, 0}, {0, 0}, {14, 18}, {0, 0}, {0, 0},
    {0, 0}, {0, 0}, {0, 0},   {0, 0},   {0, 0},   {0, 0}, {0, 0}, {-1, -1}, {0, 0}, {0, 0},
    {0, 0}, {0, 0},
};

static inline int is_resolution_class_iii(int r) {
  return r % 2;
}
static inline int h3_get_resolution(H3Index h) {
  return int((h & H3_RES_MASK_EXACT) >> H3_RES_OFFSET);
}
static inline Direction h3_get_index_digit(H3Index h, int res) {
  return Direction((h >> ((MAX_H3_RES - res) * H3_PER_DIGIT_OFFSET)) & H3_DIGIT_MASK);
}
static inline void h3_set_mode(H3Index& h, int mode) {
  h = ((h & H3_MODE_MASK_NEGATIVE) | (uint64_t(mode) << H3_MODE_OFFSET));
}
static inline void h3_set_resolution(H3Index& h, int res) {
  h = ((h & H3_RES_MASK_NEGATIVE) | (uint64_t(res) << H3_RES_OFFSET));
}
static inline void h3_set_base_cell(H3Index& h, int base_cell) {
  h = ((h & H3_BC_MASK_NEGATIVE) | (uint64_t(base_cell) << H3_BC_OFFSET));
}
static inline void h3_set_index_digit(H3Index& h, int res, Direction digit) {
  const uint64_t shift = uint64_t((MAX_H3_RES - res) * H3_PER_DIGIT_OFFSET);
  h = ((h & ~(H3_DIGIT_MASK << shift)) | (uint64_t(digit) << shift));
}

static inline int round_to_int(double x) {
  return int(x >= 0.0 ? x + 0.5 : x - 0.5);
}

static inline double square(double x) {
  return x * x;
}
static inline double point_square_dist(const Vec3d& a, const Vec3d& b) {
  return square(a.x - b.x) + square(a.y - b.y) + square(a.z - b.z);
}
static inline Vec3d geo_to_vec3d(const LatLng& geo) {
  double sin_lat;
  double cos_lat;
  double sin_lng;
  double cos_lng;
  sincos_pi_range(geo.lat, sin_lat, cos_lat);
  sincos_pi_range(geo.lng, sin_lng, cos_lng);
  Vec3d out;
  out.x = cos_lng * cos_lat;
  out.y = sin_lng * cos_lat;
  out.z = sin_lat;
  return out;
}

static inline void geo_to_closest_face_from_vec(const Vec3d& v3d, int& face, double& sqd) {
  face = 0;
  sqd = 5.0;
  for (int f = 0; f < NUM_ICOSA_FACES; ++f) {
    double sqd_t = point_square_dist(faceCenterPoint[f], v3d);
    if (sqd_t < sqd) {
      face = f;
      sqd = sqd_t;
    }
  }
}
static inline void geo_to_closest_face(const LatLng& g, int& face, double& sqd) {
  geo_to_closest_face_from_vec(geo_to_vec3d(g), face, sqd);
}

static inline int ijk_matches(const CoordIJK& a, const CoordIJK& b) {
  return a.i == b.i && a.j == b.j && a.k == b.k;
}
static inline void ijk_add(const CoordIJK& a, const CoordIJK& b, CoordIJK& out) {
  out.i = a.i + b.i;
  out.j = a.j + b.j;
  out.k = a.k + b.k;
}
static inline void ijk_sub(const CoordIJK& a, const CoordIJK& b, CoordIJK& out) {
  out.i = a.i - b.i;
  out.j = a.j - b.j;
  out.k = a.k - b.k;
}
static inline void ijk_scale(CoordIJK& c, int factor) {
  c.i *= factor;
  c.j *= factor;
  c.k *= factor;
}
static inline void ijk_normalize(CoordIJK& c) {
  if (c.i < 0) {
    c.j -= c.i;
    c.k -= c.i;
    c.i = 0;
  }
  if (c.j < 0) {
    c.i -= c.j;
    c.k -= c.j;
    c.j = 0;
  }
  if (c.k < 0) {
    c.i -= c.k;
    c.j -= c.k;
    c.k = 0;
  }
  int min = c.i;
  if (c.j < min)
    min = c.j;
  if (c.k < min)
    min = c.k;
  if (min > 0) {
    c.i -= min;
    c.j -= min;
    c.k -= min;
  }
}

static inline void hex2d_to_coord_ijk(const Vec2d& v, CoordIJK& h) {
  double a1 = std::fabs(v.x);
  double a2 = std::fabs(v.y);
  double x2 = a2 * M_RSIN60;
  double x1 = a1 + x2 / 2.0;
  int m1 = int(x1);
  int m2 = int(x2);
  double r1 = x1 - m1;
  double r2 = x2 - m2;
  h.k = 0;

  if (r1 < 0.5) {
    if (r1 < 1.0 / 3.0) {
      if (r2 < (1.0 + r1) / 2.0) {
        h.i = m1;
        h.j = m2;
      } else {
        h.i = m1;
        h.j = m2 + 1;
      }
    } else {
      h.j = (r2 < (1.0 - r1)) ? m2 : m2 + 1;
      h.i = ((1.0 - r1) <= r2 && r2 < (2.0 * r1)) ? m1 + 1 : m1;
    }
  } else {
    if (r1 < 2.0 / 3.0) {
      h.j = (r2 < (1.0 - r1)) ? m2 : m2 + 1;
      h.i = ((2.0 * r1 - 1.0) < r2 && r2 < (1.0 - r1)) ? m1 : m1 + 1;
    } else {
      if (r2 < (r1 / 2.0)) {
        h.i = m1 + 1;
        h.j = m2;
      } else {
        h.i = m1 + 1;
        h.j = m2 + 1;
      }
    }
  }

  if (v.x < 0.0) {
    if ((h.j % 2) == 0) {
      long long axisi = h.j / 2;
      long long diff = h.i - axisi;
      h.i = int(h.i - 2.0 * diff);
    } else {
      long long axisi = (h.j + 1) / 2;
      long long diff = h.i - axisi;
      h.i = int(h.i - (2.0 * diff + 1));
    }
  }

  if (v.y < 0.0) {
    h.i = h.i - (2 * h.j + 1) / 2;
    h.j = -1 * h.j;
  }

  ijk_normalize(h);
}

static inline Direction unit_ijk_to_digit(const CoordIJK& ijk) {
  CoordIJK c = ijk;
  ijk_normalize(c);
  for (int i = CENTER_DIGIT; i < NUM_DIGITS; i++) {
    if (ijk_matches(c, UNIT_VECS[i]))
      return Direction(i);
  }
  return INVALID_DIGIT;
}

static inline void up_ap7(CoordIJK& ijk) {
  int i = ijk.i - ijk.k;
  int j = ijk.j - ijk.k;
  ijk.i = round_to_int((3 * i - j) * M_ONESEVENTH);
  ijk.j = round_to_int((i + 2 * j) * M_ONESEVENTH);
  ijk.k = 0;
  ijk_normalize(ijk);
}
static inline void up_ap7r(CoordIJK& ijk) {
  int i = ijk.i - ijk.k;
  int j = ijk.j - ijk.k;
  ijk.i = round_to_int((2 * i + j) * M_ONESEVENTH);
  ijk.j = round_to_int((3 * j - i) * M_ONESEVENTH);
  ijk.k = 0;
  ijk_normalize(ijk);
}
static inline void down_ap7(CoordIJK& ijk) {
  CoordIJK i_vec;
  i_vec.i = 3;
  i_vec.j = 0;
  i_vec.k = 1;
  CoordIJK j_vec;
  j_vec.i = 1;
  j_vec.j = 3;
  j_vec.k = 0;
  CoordIJK k_vec;
  k_vec.i = 0;
  k_vec.j = 1;
  k_vec.k = 3;
  ijk_scale(i_vec, ijk.i);
  ijk_scale(j_vec, ijk.j);
  ijk_scale(k_vec, ijk.k);
  ijk_add(i_vec, j_vec, ijk);
  ijk_add(ijk, k_vec, ijk);
  ijk_normalize(ijk);
}
static inline void down_ap7r(CoordIJK& ijk) {
  CoordIJK i_vec;
  i_vec.i = 3;
  i_vec.j = 1;
  i_vec.k = 0;
  CoordIJK j_vec;
  j_vec.i = 0;
  j_vec.j = 3;
  j_vec.k = 1;
  CoordIJK k_vec;
  k_vec.i = 1;
  k_vec.j = 0;
  k_vec.k = 3;
  ijk_scale(i_vec, ijk.i);
  ijk_scale(j_vec, ijk.j);
  ijk_scale(k_vec, ijk.k);
  ijk_add(i_vec, j_vec, ijk);
  ijk_add(ijk, k_vec, ijk);
  ijk_normalize(ijk);
}

static inline Direction rotate60ccw(Direction digit) {
  switch (digit) {
    case K_AXES_DIGIT:
      return IK_AXES_DIGIT;
    case IK_AXES_DIGIT:
      return I_AXES_DIGIT;
    case I_AXES_DIGIT:
      return IJ_AXES_DIGIT;
    case IJ_AXES_DIGIT:
      return J_AXES_DIGIT;
    case J_AXES_DIGIT:
      return JK_AXES_DIGIT;
    case JK_AXES_DIGIT:
      return K_AXES_DIGIT;
    default:
      return digit;
  }
}
static inline Direction rotate60cw(Direction digit) {
  switch (digit) {
    case K_AXES_DIGIT:
      return JK_AXES_DIGIT;
    case JK_AXES_DIGIT:
      return J_AXES_DIGIT;
    case J_AXES_DIGIT:
      return IJ_AXES_DIGIT;
    case IJ_AXES_DIGIT:
      return I_AXES_DIGIT;
    case I_AXES_DIGIT:
      return IK_AXES_DIGIT;
    case IK_AXES_DIGIT:
      return K_AXES_DIGIT;
    default:
      return digit;
  }
}

static inline Direction h3_leading_non_zero_digit(H3Index h) {
  for (int r = 1, res = h3_get_resolution(h); r <= res; r++) {
    Direction d = h3_get_index_digit(h, r);
    if (d != CENTER_DIGIT)
      return d;
  }
  return CENTER_DIGIT;
}
static inline H3Index h3_rotate60ccw(H3Index h) {
  for (int r = 1, res = h3_get_resolution(h); r <= res; r++) {
    h3_set_index_digit(h, r, rotate60ccw(h3_get_index_digit(h, r)));
  }
  return h;
}
static inline H3Index h3_rotate60cw(H3Index h) {
  for (int r = 1, res = h3_get_resolution(h); r <= res; r++) {
    h3_set_index_digit(h, r, rotate60cw(h3_get_index_digit(h, r)));
  }
  return h;
}
static inline H3Index h3_rotate_pent60ccw(H3Index h) {
  int found = 0;
  for (int r = 1, res = h3_get_resolution(h); r <= res; r++) {
    h3_set_index_digit(h, r, rotate60ccw(h3_get_index_digit(h, r)));
    if (!found && h3_get_index_digit(h, r) != CENTER_DIGIT) {
      found = 1;
      if (h3_leading_non_zero_digit(h) == K_AXES_DIGIT)
        h = h3_rotate60ccw(h);
    }
  }
  return h;
}

static inline int is_base_cell_pentagon(int base_cell) {
  return base_cell >= 0 && base_cell < NUM_BASE_CELLS && baseCellIsPentagon[base_cell] != 0;
}
static inline bool base_cell_is_cw_offset(int base_cell, int test_face) {
  return baseCellCwOffsetPent[base_cell][0] == test_face ||
         baseCellCwOffsetPent[base_cell][1] == test_face;
}
static inline int face_ijk_to_base_cell(const FaceIJK& h) {
  return faceIjkBaseCell[((h.face * 3 + h.coord.i) * 3 + h.coord.j) * 3 + h.coord.k];
}
static inline int face_ijk_to_base_cell_ccw_rot60(const FaceIJK& h) {
  return faceIjkBaseCellRot[((h.face * 3 + h.coord.i) * 3 + h.coord.j) * 3 + h.coord.k];
}

static inline void geo_to_hex2d(const LatLng& g, int res, int& face, Vec2d& v) {
  const Vec3d g_vec = geo_to_vec3d(g);
  double sqd;
  geo_to_closest_face_from_vec(g_vec, face, sqd);
  if (sqd < 1.0e-24) {
    v.x = 0.0;
    v.y = 0.0;
    return;
  }

  double cos_r = 1.0 - sqd * 0.5;
  if (cos_r < 1.0e-12)
    cos_r = 1.0e-12;
  if (cos_r > 1.0)
    cos_r = 1.0;
  double sin2_r = 1.0 - cos_r * cos_r;
  if (sin2_r < 0.0)
    sin2_r = 0.0;
  double r = (std::sqrt(sin2_r) / cos_r) * RES_SCALE[res];

  const double sin_lat = g_vec.z;
  const double cos_lat = std::sqrt(g_vec.x * g_vec.x + g_vec.y * g_vec.y);
  double sin_lng = 0.0;
  double cos_lng = 1.0;
  if (cos_lat > 1.0e-24) {
    sin_lng = g_vec.y / cos_lat;
    cos_lng = g_vec.x / cos_lat;
  }

  const double sin_dlng = sin_lng * faceCenterLngCos[face] - cos_lng * faceCenterLngSin[face];
  const double cos_dlng = cos_lng * faceCenterLngCos[face] + sin_lng * faceCenterLngSin[face];
  const double az_y = cos_lat * sin_dlng;
  const double az_x =
      faceCenterLatCos[face] * sin_lat - faceCenterLatSin[face] * cos_lat * cos_dlng;
  double az_len = std::sqrt(az_x * az_x + az_y * az_y);
  if (az_len < 1.0e-24) {
    v.x = 0.0;
    v.y = 0.0;
    return;
  }

  double unit_x = (faceAxesAzRadsCII0Cos[face] * az_x + faceAxesAzRadsCII0Sin[face] * az_y) /
                  az_len;
  double unit_y = (faceAxesAzRadsCII0Sin[face] * az_x - faceAxesAzRadsCII0Cos[face] * az_y) /
                  az_len;
  if (is_resolution_class_iii(res)) {
    const double rot_x = unit_x * M_AP7_ROT_COS + unit_y * M_AP7_ROT_SIN;
    const double rot_y = unit_y * M_AP7_ROT_COS - unit_x * M_AP7_ROT_SIN;
    unit_x = rot_x;
    unit_y = rot_y;
  }

  v.x = r * unit_x;
  v.y = r * unit_y;
}

static inline FaceIJK geo_to_face_ijk(const LatLng& g, int res) {
  FaceIJK h;
  Vec2d v;
  geo_to_hex2d(g, res, h.face, v);
  hex2d_to_coord_ijk(v, h.coord);
  return h;
}

static inline H3Index face_ijk_to_h3(const FaceIJK& fijk, int res) {
  H3Index h = H3_INIT;
  h3_set_mode(h, H3_CELL_MODE);
  h3_set_resolution(h, res);

  if (res == 0) {
    if (fijk.coord.i > 2 || fijk.coord.j > 2 || fijk.coord.k > 2)
      return 0;
    h3_set_base_cell(h, face_ijk_to_base_cell(fijk));
    return h;
  }

  FaceIJK fijk_bc = fijk;
  CoordIJK& ijk = fijk_bc.coord;
  for (int r = res - 1; r >= 0; r--) {
    CoordIJK last_ijk = ijk;
    CoordIJK last_center;
    if (is_resolution_class_iii(r + 1)) {
      up_ap7(ijk);
      last_center = ijk;
      down_ap7(last_center);
    } else {
      up_ap7r(ijk);
      last_center = ijk;
      down_ap7r(last_center);
    }
    CoordIJK diff;
    ijk_sub(last_ijk, last_center, diff);
    ijk_normalize(diff);
    h3_set_index_digit(h, r + 1, unit_ijk_to_digit(diff));
  }

  if (fijk_bc.coord.i > 2 || fijk_bc.coord.j > 2 || fijk_bc.coord.k > 2)
    return 0;

  int base_cell = face_ijk_to_base_cell(fijk_bc);
  h3_set_base_cell(h, base_cell);

  int num_rots = face_ijk_to_base_cell_ccw_rot60(fijk_bc);
  if (is_base_cell_pentagon(base_cell)) {
    if (h3_leading_non_zero_digit(h) == K_AXES_DIGIT) {
      if (base_cell_is_cw_offset(base_cell, fijk_bc.face))
        h = h3_rotate60cw(h);
      else
        h = h3_rotate60ccw(h);
    }
    for (int i = 0; i < num_rots; i++)
      h = h3_rotate_pent60ccw(h);
  } else {
    for (int i = 0; i < num_rots; i++)
      h = h3_rotate60ccw(h);
  }
  return h;
}

struct CellResult {
  H3Index cell;
  uint8_t valid;
};

static inline CellResult lat_lng_to_cell_degs(double lat_deg, double lng_deg, int res) {
  CellResult invalid;
  invalid.cell = 0;
  invalid.valid = 0;
  if (res < 0 || res > MAX_H3_RES)
    return invalid;
  if (!(lat_deg >= -90.0 && lat_deg <= 90.0 && lng_deg >= -180.0 && lng_deg <= 180.0))
    return invalid;
  LatLng g;
  g.lat = lat_deg * M_PI_180;
  g.lng = lng_deg * M_PI_180;
  FaceIJK fijk = geo_to_face_ijk(g, res);
  H3Index out = face_ijk_to_h3(fijk, res);
  CellResult result;
  result.cell = out;
  result.valid = out == 0 ? uint8_t(0) : uint8_t(1);
  return result;
}

}  // namespace pgaccel_h3_exact

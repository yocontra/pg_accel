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
static constexpr double M_2PI_LOCAL = 6.28318530717958647692528676655900576839433;
static constexpr double M_PI_180 = 0.0174532925199432957692369076848861271111;
static constexpr double EPSILON = 0.0000000000000001;
static constexpr double M_RSIN60 = 1.1547005383792515290182975610039149112953;
static constexpr double M_ONESEVENTH = 0.14285714285714285714285714285714285;
static constexpr double M_AP7_ROT_RADS = 0.333473172251832115336090755351601070065900389;
static constexpr double INV_RES0_U_GNOMONIC = 2.61803398874989588842;
static constexpr double M_SQRT7 = 2.6457513110645905905016157536392604257102;

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

static constexpr LatLng faceCenterGeo[NUM_ICOSA_FACES] = {
    {0.803582649718989942, 1.248397419617396099},    // face  0
    {1.307747883455638156, 2.536945009877921159},    // face  1
    {1.054751253523952054, -1.347517358900396623},   // face  2
    {0.600191595538186799, -0.450603909469755746},   // face  3
    {0.491715428198773866, 0.401988202911306943},    // face  4
    {0.172745327415618701, 1.678146885280433686},    // face  5
    {0.605929321571350690, 2.953923329812411617},    // face  6
    {0.427370518328979641, -1.888876200336285401},   // face  7
    {-0.079066118549212831, -0.733429513380867741},  // face  8
    {-0.230961644455383637, 0.506495587332349035},   // face  9
    {0.079066118549212831, 2.408163140208925497},    // face 10
    {0.230961644455383637, -2.635097066257444203},   // face 11
    {-0.172745327415618701, -1.463445768309359553},  // face 12
    {-0.605929321571350690, -0.187669323777381622},  // face 13
    {-0.427370518328979641, 1.252716453253507838},   // face 14
    {-0.600191595538186799, 2.690988744120037492},   // face 15
    {-0.491715428198773866, -2.739604450678486295},  // face 16
    {-0.803582649718989942, -1.893195233972397139},  // face 17
    {-1.307747883455638156, -0.604647643711872080},  // face 18
    {-1.054751253523952054, 1.794075294689396615},   // face 19
};

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

static constexpr double faceAxesAzRadsCII[NUM_ICOSA_FACES][3] = {
    {5.619958268523939882, 3.525563166130744542, 1.431168063737548730},  // face  0
    {5.760339081714187279, 3.665943979320991689, 1.571548876927796127},  // face  1
    {0.780213654393430055, 4.969003859179821079, 2.874608756786625655},  // face  2
    {0.430469363979999913, 4.619259568766391033, 2.524864466373195467},  // face  3
    {6.130269123335111400, 4.035874020941915804, 1.941478918548720291},  // face  4
    {2.692877706530642877, 0.598482604137447119, 4.787272808923838195},  // face  5
    {2.982963003477243874, 0.888567901084048369, 5.077358105870439581},  // face  6
    {3.532912002790141181, 1.438516900396945656, 5.627307105183336758},  // face  7
    {3.494305004259568154, 1.399909901866372864, 5.588700106652763840},  // face  8
    {3.003214169499538391, 0.908819067106342928, 5.097609271892733906},  // face  9
    {5.930472956509811562, 3.836077854116615875, 1.741682751723420374},  // face 10
    {0.138378484090254847, 4.327168688876645809, 2.232773586483450311},  // face 11
    {0.448714947059150361, 4.637505151845541521, 2.543110049452346120},  // face 12
    {0.158629650112549365, 4.347419854898940135, 2.253024752505744869},  // face 13
    {5.891865957979238535, 3.797470855586042958, 1.703075753192847583},  // face 14
    {2.711123289609793325, 0.616728187216597771, 4.805518392002988683},  // face 15
    {3.294508837434268316, 1.200113735041072948, 5.388903939827463911},  // face 16
    {3.804819692245439833, 1.710424589852244509, 5.899214794638635174},  // face 17
    {3.664438879055192436, 1.570043776661997111, 5.758833981448388027},  // face 18
    {2.361378999196363184, 0.266983896803167583, 4.455774101589558636},  // face 19
};

static constexpr BaseCellRotation faceIjkBaseCells[NUM_ICOSA_FACES][3][3][3] = {
    {// face 0
     {
         // i 0
         {{16, 0}, {18, 0}, {24, 0}},  // j 0
         {{33, 0}, {30, 0}, {32, 3}},  // j 1
         {{49, 1}, {48, 3}, {50, 3}}   // j 2
     },
     {
         // i 1
         {{8, 0}, {5, 5}, {10, 5}},    // j 0
         {{22, 0}, {16, 0}, {18, 0}},  // j 1
         {{41, 1}, {33, 0}, {30, 0}}   // j 2
     },
     {
         // i 2
         {{4, 0}, {0, 5}, {2, 5}},    // j 0
         {{15, 1}, {8, 0}, {5, 5}},   // j 1
         {{31, 1}, {22, 0}, {16, 0}}  // j 2
     }},
    {// face 1
     {
         // i 0
         {{2, 0}, {6, 0}, {14, 0}},    // j 0
         {{10, 0}, {11, 0}, {17, 3}},  // j 1
         {{24, 1}, {23, 3}, {25, 3}}   // j 2
     },
     {
         // i 1
         {{0, 0}, {1, 5}, {9, 5}},    // j 0
         {{5, 0}, {2, 0}, {6, 0}},    // j 1
         {{18, 1}, {10, 0}, {11, 0}}  // j 2
     },
     {
         // i 2
         {{4, 1}, {3, 5}, {7, 5}},  // j 0
         {{8, 1}, {0, 0}, {1, 5}},  // j 1
         {{16, 1}, {5, 0}, {2, 0}}  // j 2
     }},
    {// face 2
     {
         // i 0
         {{7, 0}, {21, 0}, {38, 0}},  // j 0
         {{9, 0}, {19, 0}, {34, 3}},  // j 1
         {{14, 1}, {20, 3}, {36, 3}}  // j 2
     },
     {
         // i 1
         {{3, 0}, {13, 5}, {29, 5}},  // j 0
         {{1, 0}, {7, 0}, {21, 0}},   // j 1
         {{6, 1}, {9, 0}, {19, 0}}    // j 2
     },
     {
         // i 2
         {{4, 2}, {12, 5}, {26, 5}},  // j 0
         {{0, 1}, {3, 0}, {13, 5}},   // j 1
         {{2, 1}, {1, 0}, {7, 0}}     // j 2
     }},
    {// face 3
     {
         // i 0
         {{26, 0}, {42, 0}, {58, 0}},  // j 0
         {{29, 0}, {43, 0}, {62, 3}},  // j 1
         {{38, 1}, {47, 3}, {64, 3}}   // j 2
     },
     {
         // i 1
         {{12, 0}, {28, 5}, {44, 5}},  // j 0
         {{13, 0}, {26, 0}, {42, 0}},  // j 1
         {{21, 1}, {29, 0}, {43, 0}}   // j 2
     },
     {
         // i 2
         {{4, 3}, {15, 5}, {31, 5}},  // j 0
         {{3, 1}, {12, 0}, {28, 5}},  // j 1
         {{7, 1}, {13, 0}, {26, 0}}   // j 2
     }},
    {// face 4
     {
         // i 0
         {{31, 0}, {41, 0}, {49, 0}},  // j 0
         {{44, 0}, {53, 0}, {61, 3}},  // j 1
         {{58, 1}, {65, 3}, {75, 3}}   // j 2
     },
     {
         // i 1
         {{15, 0}, {22, 5}, {33, 5}},  // j 0
         {{28, 0}, {31, 0}, {41, 0}},  // j 1
         {{42, 1}, {44, 0}, {53, 0}}   // j 2
     },
     {
         // i 2
         {{4, 4}, {8, 5}, {16, 5}},    // j 0
         {{12, 1}, {15, 0}, {22, 5}},  // j 1
         {{26, 1}, {28, 0}, {31, 0}}   // j 2
     }},
    {// face 5
     {
         // i 0
         {{50, 0}, {48, 0}, {49, 3}},  // j 0
         {{32, 0}, {30, 3}, {33, 3}},  // j 1
         {{24, 3}, {18, 3}, {16, 3}}   // j 2
     },
     {
         // i 1
         {{70, 0}, {67, 0}, {66, 3}},  // j 0
         {{52, 3}, {50, 0}, {48, 0}},  // j 1
         {{37, 3}, {32, 0}, {30, 3}}   // j 2
     },
     {
         // i 2
         {{83, 0}, {87, 3}, {85, 3}},  // j 0
         {{74, 3}, {70, 0}, {67, 0}},  // j 1
         {{57, 1}, {52, 3}, {50, 0}}   // j 2
     }},
    {// face 6
     {
         // i 0
         {{25, 0}, {23, 0}, {24, 3}},  // j 0
         {{17, 0}, {11, 3}, {10, 3}},  // j 1
         {{14, 3}, {6, 3}, {2, 3}}     // j 2
     },
     {
         // i 1
         {{45, 0}, {39, 0}, {37, 3}},  // j 0
         {{35, 3}, {25, 0}, {23, 0}},  // j 1
         {{27, 3}, {17, 0}, {11, 3}}   // j 2
     },
     {
         // i 2
         {{63, 0}, {59, 3}, {57, 3}},  // j 0
         {{56, 3}, {45, 0}, {39, 0}},  // j 1
         {{46, 3}, {35, 3}, {25, 0}}   // j 2
     }},
    {// face 7
     {
         // i 0
         {{36, 0}, {20, 0}, {14, 3}},  // j 0
         {{34, 0}, {19, 3}, {9, 3}},   // j 1
         {{38, 3}, {21, 3}, {7, 3}}    // j 2
     },
     {
         // i 1
         {{55, 0}, {40, 0}, {27, 3}},  // j 0
         {{54, 3}, {36, 0}, {20, 0}},  // j 1
         {{51, 3}, {34, 0}, {19, 3}}   // j 2
     },
     {
         // i 2
         {{72, 0}, {60, 3}, {46, 3}},  // j 0
         {{73, 3}, {55, 0}, {40, 0}},  // j 1
         {{71, 3}, {54, 3}, {36, 0}}   // j 2
     }},
    {// face 8
     {
         // i 0
         {{64, 0}, {47, 0}, {38, 3}},  // j 0
         {{62, 0}, {43, 3}, {29, 3}},  // j 1
         {{58, 3}, {42, 3}, {26, 3}}   // j 2
     },
     {
         // i 1
         {{84, 0}, {69, 0}, {51, 3}},  // j 0
         {{82, 3}, {64, 0}, {47, 0}},  // j 1
         {{76, 3}, {62, 0}, {43, 3}}   // j 2
     },
     {
         // i 2
         {{97, 0}, {89, 3}, {71, 3}},  // j 0
         {{98, 3}, {84, 0}, {69, 0}},  // j 1
         {{96, 3}, {82, 3}, {64, 0}}   // j 2
     }},
    {// face 9
     {
         // i 0
         {{75, 0}, {65, 0}, {58, 3}},  // j 0
         {{61, 0}, {53, 3}, {44, 3}},  // j 1
         {{49, 3}, {41, 3}, {31, 3}}   // j 2
     },
     {
         // i 1
         {{94, 0}, {86, 0}, {76, 3}},  // j 0
         {{81, 3}, {75, 0}, {65, 0}},  // j 1
         {{66, 3}, {61, 0}, {53, 3}}   // j 2
     },
     {
         // i 2
         {{107, 0}, {104, 3}, {96, 3}},  // j 0
         {{101, 3}, {94, 0}, {86, 0}},   // j 1
         {{85, 3}, {81, 3}, {75, 0}}     // j 2
     }},
    {// face 10
     {
         // i 0
         {{57, 0}, {59, 0}, {63, 3}},  // j 0
         {{74, 0}, {78, 3}, {79, 3}},  // j 1
         {{83, 3}, {92, 3}, {95, 3}}   // j 2
     },
     {
         // i 1
         {{37, 0}, {39, 3}, {45, 3}},  // j 0
         {{52, 0}, {57, 0}, {59, 0}},  // j 1
         {{70, 3}, {74, 0}, {78, 3}}   // j 2
     },
     {
         // i 2
         {{24, 0}, {23, 3}, {25, 3}},  // j 0
         {{32, 3}, {37, 0}, {39, 3}},  // j 1
         {{50, 3}, {52, 0}, {57, 0}}   // j 2
     }},
    {// face 11
     {
         // i 0
         {{46, 0}, {60, 0}, {72, 3}},  // j 0
         {{56, 0}, {68, 3}, {80, 3}},  // j 1
         {{63, 3}, {77, 3}, {90, 3}}   // j 2
     },
     {
         // i 1
         {{27, 0}, {40, 3}, {55, 3}},  // j 0
         {{35, 0}, {46, 0}, {60, 0}},  // j 1
         {{45, 3}, {56, 0}, {68, 3}}   // j 2
     },
     {
         // i 2
         {{14, 0}, {20, 3}, {36, 3}},  // j 0
         {{17, 3}, {27, 0}, {40, 3}},  // j 1
         {{25, 3}, {35, 0}, {46, 0}}   // j 2
     }},
    {// face 12
     {
         // i 0
         {{71, 0}, {89, 0}, {97, 3}},   // j 0
         {{73, 0}, {91, 3}, {103, 3}},  // j 1
         {{72, 3}, {88, 3}, {105, 3}}   // j 2
     },
     {
         // i 1
         {{51, 0}, {69, 3}, {84, 3}},  // j 0
         {{54, 0}, {71, 0}, {89, 0}},  // j 1
         {{55, 3}, {73, 0}, {91, 3}}   // j 2
     },
     {
         // i 2
         {{38, 0}, {47, 3}, {64, 3}},  // j 0
         {{34, 3}, {51, 0}, {69, 3}},  // j 1
         {{36, 3}, {54, 0}, {71, 0}}   // j 2
     }},
    {// face 13
     {
         // i 0
         {{96, 0}, {104, 0}, {107, 3}},  // j 0
         {{98, 0}, {110, 3}, {115, 3}},  // j 1
         {{97, 3}, {111, 3}, {119, 3}}   // j 2
     },
     {
         // i 1
         {{76, 0}, {86, 3}, {94, 3}},   // j 0
         {{82, 0}, {96, 0}, {104, 0}},  // j 1
         {{84, 3}, {98, 0}, {110, 3}}   // j 2
     },
     {
         // i 2
         {{58, 0}, {65, 3}, {75, 3}},  // j 0
         {{62, 3}, {76, 0}, {86, 3}},  // j 1
         {{64, 3}, {82, 0}, {96, 0}}   // j 2
     }},
    {// face 14
     {
         // i 0
         {{85, 0}, {87, 0}, {83, 3}},     // j 0
         {{101, 0}, {102, 3}, {100, 3}},  // j 1
         {{107, 3}, {112, 3}, {114, 3}}   // j 2
     },
     {
         // i 1
         {{66, 0}, {67, 3}, {70, 3}},   // j 0
         {{81, 0}, {85, 0}, {87, 0}},   // j 1
         {{94, 3}, {101, 0}, {102, 3}}  // j 2
     },
     {
         // i 2
         {{49, 0}, {48, 3}, {50, 3}},  // j 0
         {{61, 3}, {66, 0}, {67, 3}},  // j 1
         {{75, 3}, {81, 0}, {85, 0}}   // j 2
     }},
    {// face 15
     {
         // i 0
         {{95, 0}, {92, 0}, {83, 0}},  // j 0
         {{79, 0}, {78, 0}, {74, 3}},  // j 1
         {{63, 1}, {59, 3}, {57, 3}}   // j 2
     },
     {
         // i 1
         {{109, 0}, {108, 0}, {100, 5}},  // j 0
         {{93, 1}, {95, 0}, {92, 0}},     // j 1
         {{77, 1}, {79, 0}, {78, 0}}      // j 2
     },
     {
         // i 2
         {{117, 4}, {118, 5}, {114, 5}},  // j 0
         {{106, 1}, {109, 0}, {108, 0}},  // j 1
         {{90, 1}, {93, 1}, {95, 0}}      // j 2
     }},
    {// face 16
     {
         // i 0
         {{90, 0}, {77, 0}, {63, 0}},  // j 0
         {{80, 0}, {68, 0}, {56, 3}},  // j 1
         {{72, 1}, {60, 3}, {46, 3}}   // j 2
     },
     {
         // i 1
         {{106, 0}, {93, 0}, {79, 5}},  // j 0
         {{99, 1}, {90, 0}, {77, 0}},   // j 1
         {{88, 1}, {80, 0}, {68, 0}}    // j 2
     },
     {
         // i 2
         {{117, 3}, {109, 5}, {95, 5}},  // j 0
         {{113, 1}, {106, 0}, {93, 0}},  // j 1
         {{105, 1}, {99, 1}, {90, 0}}    // j 2
     }},
    {// face 17
     {
         // i 0
         {{105, 0}, {88, 0}, {72, 0}},  // j 0
         {{103, 0}, {91, 0}, {73, 3}},  // j 1
         {{97, 1}, {89, 3}, {71, 3}}    // j 2
     },
     {
         // i 1
         {{113, 0}, {99, 0}, {80, 5}},   // j 0
         {{116, 1}, {105, 0}, {88, 0}},  // j 1
         {{111, 1}, {103, 0}, {91, 0}}   // j 2
     },
     {
         // i 2
         {{117, 2}, {106, 5}, {90, 5}},  // j 0
         {{121, 1}, {113, 0}, {99, 0}},  // j 1
         {{119, 1}, {116, 1}, {105, 0}}  // j 2
     }},
    {// face 18
     {
         // i 0
         {{119, 0}, {111, 0}, {97, 0}},  // j 0
         {{115, 0}, {110, 0}, {98, 3}},  // j 1
         {{107, 1}, {104, 3}, {96, 3}}   // j 2
     },
     {
         // i 1
         {{121, 0}, {116, 0}, {103, 5}},  // j 0
         {{120, 1}, {119, 0}, {111, 0}},  // j 1
         {{112, 1}, {115, 0}, {110, 0}}   // j 2
     },
     {
         // i 2
         {{117, 1}, {113, 5}, {105, 5}},  // j 0
         {{118, 1}, {121, 0}, {116, 0}},  // j 1
         {{114, 1}, {120, 1}, {119, 0}}   // j 2
     }},
    {// face 19
     {
         // i 0
         {{114, 0}, {112, 0}, {107, 0}},  // j 0
         {{100, 0}, {102, 0}, {101, 3}},  // j 1
         {{83, 1}, {87, 3}, {85, 3}}      // j 2
     },
     {
         // i 1
         {{118, 0}, {120, 0}, {115, 5}},  // j 0
         {{108, 1}, {114, 0}, {112, 0}},  // j 1
         {{92, 1}, {100, 0}, {102, 0}}    // j 2
     },
     {
         // i 2
         {{117, 0}, {121, 5}, {119, 5}},  // j 0
         {{109, 1}, {118, 0}, {120, 0}},  // j 1
         {{95, 1}, {108, 1}, {114, 0}}    // j 2
     }}};

static constexpr BaseCellData baseCellData[NUM_BASE_CELLS] = {

    {{1, {1, 0, 0}}, 0, {0, 0}},     // base cell 0
    {{2, {1, 1, 0}}, 0, {0, 0}},     // base cell 1
    {{1, {0, 0, 0}}, 0, {0, 0}},     // base cell 2
    {{2, {1, 0, 0}}, 0, {0, 0}},     // base cell 3
    {{0, {2, 0, 0}}, 1, {-1, -1}},   // base cell 4
    {{1, {1, 1, 0}}, 0, {0, 0}},     // base cell 5
    {{1, {0, 0, 1}}, 0, {0, 0}},     // base cell 6
    {{2, {0, 0, 0}}, 0, {0, 0}},     // base cell 7
    {{0, {1, 0, 0}}, 0, {0, 0}},     // base cell 8
    {{2, {0, 1, 0}}, 0, {0, 0}},     // base cell 9
    {{1, {0, 1, 0}}, 0, {0, 0}},     // base cell 10
    {{1, {0, 1, 1}}, 0, {0, 0}},     // base cell 11
    {{3, {1, 0, 0}}, 0, {0, 0}},     // base cell 12
    {{3, {1, 1, 0}}, 0, {0, 0}},     // base cell 13
    {{11, {2, 0, 0}}, 1, {2, 6}},    // base cell 14
    {{4, {1, 0, 0}}, 0, {0, 0}},     // base cell 15
    {{0, {0, 0, 0}}, 0, {0, 0}},     // base cell 16
    {{6, {0, 1, 0}}, 0, {0, 0}},     // base cell 17
    {{0, {0, 0, 1}}, 0, {0, 0}},     // base cell 18
    {{2, {0, 1, 1}}, 0, {0, 0}},     // base cell 19
    {{7, {0, 0, 1}}, 0, {0, 0}},     // base cell 20
    {{2, {0, 0, 1}}, 0, {0, 0}},     // base cell 21
    {{0, {1, 1, 0}}, 0, {0, 0}},     // base cell 22
    {{6, {0, 0, 1}}, 0, {0, 0}},     // base cell 23
    {{10, {2, 0, 0}}, 1, {1, 5}},    // base cell 24
    {{6, {0, 0, 0}}, 0, {0, 0}},     // base cell 25
    {{3, {0, 0, 0}}, 0, {0, 0}},     // base cell 26
    {{11, {1, 0, 0}}, 0, {0, 0}},    // base cell 27
    {{4, {1, 1, 0}}, 0, {0, 0}},     // base cell 28
    {{3, {0, 1, 0}}, 0, {0, 0}},     // base cell 29
    {{0, {0, 1, 1}}, 0, {0, 0}},     // base cell 30
    {{4, {0, 0, 0}}, 0, {0, 0}},     // base cell 31
    {{5, {0, 1, 0}}, 0, {0, 0}},     // base cell 32
    {{0, {0, 1, 0}}, 0, {0, 0}},     // base cell 33
    {{7, {0, 1, 0}}, 0, {0, 0}},     // base cell 34
    {{11, {1, 1, 0}}, 0, {0, 0}},    // base cell 35
    {{7, {0, 0, 0}}, 0, {0, 0}},     // base cell 36
    {{10, {1, 0, 0}}, 0, {0, 0}},    // base cell 37
    {{12, {2, 0, 0}}, 1, {3, 7}},    // base cell 38
    {{6, {1, 0, 1}}, 0, {0, 0}},     // base cell 39
    {{7, {1, 0, 1}}, 0, {0, 0}},     // base cell 40
    {{4, {0, 0, 1}}, 0, {0, 0}},     // base cell 41
    {{3, {0, 0, 1}}, 0, {0, 0}},     // base cell 42
    {{3, {0, 1, 1}}, 0, {0, 0}},     // base cell 43
    {{4, {0, 1, 0}}, 0, {0, 0}},     // base cell 44
    {{6, {1, 0, 0}}, 0, {0, 0}},     // base cell 45
    {{11, {0, 0, 0}}, 0, {0, 0}},    // base cell 46
    {{8, {0, 0, 1}}, 0, {0, 0}},     // base cell 47
    {{5, {0, 0, 1}}, 0, {0, 0}},     // base cell 48
    {{14, {2, 0, 0}}, 1, {0, 9}},    // base cell 49
    {{5, {0, 0, 0}}, 0, {0, 0}},     // base cell 50
    {{12, {1, 0, 0}}, 0, {0, 0}},    // base cell 51
    {{10, {1, 1, 0}}, 0, {0, 0}},    // base cell 52
    {{4, {0, 1, 1}}, 0, {0, 0}},     // base cell 53
    {{12, {1, 1, 0}}, 0, {0, 0}},    // base cell 54
    {{7, {1, 0, 0}}, 0, {0, 0}},     // base cell 55
    {{11, {0, 1, 0}}, 0, {0, 0}},    // base cell 56
    {{10, {0, 0, 0}}, 0, {0, 0}},    // base cell 57
    {{13, {2, 0, 0}}, 1, {4, 8}},    // base cell 58
    {{10, {0, 0, 1}}, 0, {0, 0}},    // base cell 59
    {{11, {0, 0, 1}}, 0, {0, 0}},    // base cell 60
    {{9, {0, 1, 0}}, 0, {0, 0}},     // base cell 61
    {{8, {0, 1, 0}}, 0, {0, 0}},     // base cell 62
    {{6, {2, 0, 0}}, 1, {11, 15}},   // base cell 63
    {{8, {0, 0, 0}}, 0, {0, 0}},     // base cell 64
    {{9, {0, 0, 1}}, 0, {0, 0}},     // base cell 65
    {{14, {1, 0, 0}}, 0, {0, 0}},    // base cell 66
    {{5, {1, 0, 1}}, 0, {0, 0}},     // base cell 67
    {{16, {0, 1, 1}}, 0, {0, 0}},    // base cell 68
    {{8, {1, 0, 1}}, 0, {0, 0}},     // base cell 69
    {{5, {1, 0, 0}}, 0, {0, 0}},     // base cell 70
    {{12, {0, 0, 0}}, 0, {0, 0}},    // base cell 71
    {{7, {2, 0, 0}}, 1, {12, 16}},   // base cell 72
    {{12, {0, 1, 0}}, 0, {0, 0}},    // base cell 73
    {{10, {0, 1, 0}}, 0, {0, 0}},    // base cell 74
    {{9, {0, 0, 0}}, 0, {0, 0}},     // base cell 75
    {{13, {1, 0, 0}}, 0, {0, 0}},    // base cell 76
    {{16, {0, 0, 1}}, 0, {0, 0}},    // base cell 77
    {{15, {0, 1, 1}}, 0, {0, 0}},    // base cell 78
    {{15, {0, 1, 0}}, 0, {0, 0}},    // base cell 79
    {{16, {0, 1, 0}}, 0, {0, 0}},    // base cell 80
    {{14, {1, 1, 0}}, 0, {0, 0}},    // base cell 81
    {{13, {1, 1, 0}}, 0, {0, 0}},    // base cell 82
    {{5, {2, 0, 0}}, 1, {10, 19}},   // base cell 83
    {{8, {1, 0, 0}}, 0, {0, 0}},     // base cell 84
    {{14, {0, 0, 0}}, 0, {0, 0}},    // base cell 85
    {{9, {1, 0, 1}}, 0, {0, 0}},     // base cell 86
    {{14, {0, 0, 1}}, 0, {0, 0}},    // base cell 87
    {{17, {0, 0, 1}}, 0, {0, 0}},    // base cell 88
    {{12, {0, 0, 1}}, 0, {0, 0}},    // base cell 89
    {{16, {0, 0, 0}}, 0, {0, 0}},    // base cell 90
    {{17, {0, 1, 1}}, 0, {0, 0}},    // base cell 91
    {{15, {0, 0, 1}}, 0, {0, 0}},    // base cell 92
    {{16, {1, 0, 1}}, 0, {0, 0}},    // base cell 93
    {{9, {1, 0, 0}}, 0, {0, 0}},     // base cell 94
    {{15, {0, 0, 0}}, 0, {0, 0}},    // base cell 95
    {{13, {0, 0, 0}}, 0, {0, 0}},    // base cell 96
    {{8, {2, 0, 0}}, 1, {13, 17}},   // base cell 97
    {{13, {0, 1, 0}}, 0, {0, 0}},    // base cell 98
    {{17, {1, 0, 1}}, 0, {0, 0}},    // base cell 99
    {{19, {0, 1, 0}}, 0, {0, 0}},    // base cell 100
    {{14, {0, 1, 0}}, 0, {0, 0}},    // base cell 101
    {{19, {0, 1, 1}}, 0, {0, 0}},    // base cell 102
    {{17, {0, 1, 0}}, 0, {0, 0}},    // base cell 103
    {{13, {0, 0, 1}}, 0, {0, 0}},    // base cell 104
    {{17, {0, 0, 0}}, 0, {0, 0}},    // base cell 105
    {{16, {1, 0, 0}}, 0, {0, 0}},    // base cell 106
    {{9, {2, 0, 0}}, 1, {14, 18}},   // base cell 107
    {{15, {1, 0, 1}}, 0, {0, 0}},    // base cell 108
    {{15, {1, 0, 0}}, 0, {0, 0}},    // base cell 109
    {{18, {0, 1, 1}}, 0, {0, 0}},    // base cell 110
    {{18, {0, 0, 1}}, 0, {0, 0}},    // base cell 111
    {{19, {0, 0, 1}}, 0, {0, 0}},    // base cell 112
    {{17, {1, 0, 0}}, 0, {0, 0}},    // base cell 113
    {{19, {0, 0, 0}}, 0, {0, 0}},    // base cell 114
    {{18, {0, 1, 0}}, 0, {0, 0}},    // base cell 115
    {{18, {1, 0, 1}}, 0, {0, 0}},    // base cell 116
    {{19, {2, 0, 0}}, 1, {-1, -1}},  // base cell 117
    {{19, {1, 0, 0}}, 0, {0, 0}},    // base cell 118
    {{18, {0, 0, 0}}, 0, {0, 0}},    // base cell 119
    {{19, {1, 0, 1}}, 0, {0, 0}},    // base cell 120
    {{18, {1, 0, 0}}, 0, {0, 0}}     // base cell 121
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
  return int(x >= 0.0 ? std::floor(x + 0.5) : std::ceil(x - 0.5));
}

static inline double pos_angle_rads(double rads) {
  double tmp = ((rads < 0.0) ? rads + M_2PI_LOCAL : rads);
  if (rads >= M_2PI_LOCAL)
    tmp -= M_2PI_LOCAL;
  return tmp;
}

static inline double geo_azimuth_rads(const LatLng& p1, const LatLng& p2) {
  return std::atan2(std::cos(p2.lat) * std::sin(p2.lng - p1.lng),
                    std::cos(p1.lat) * std::sin(p2.lat) -
                        std::sin(p1.lat) * std::cos(p2.lat) * std::cos(p2.lng - p1.lng));
}

static inline double square(double x) {
  return x * x;
}
static inline double point_square_dist(const Vec3d& a, const Vec3d& b) {
  return square(a.x - b.x) + square(a.y - b.y) + square(a.z - b.z);
}
static inline Vec3d geo_to_vec3d(const LatLng& geo) {
  double r = std::cos(geo.lat);
  return {std::cos(geo.lng) * r, std::sin(geo.lng) * r, std::sin(geo.lat)};
}

static inline void geo_to_closest_face(const LatLng& g, int& face, double& sqd) {
  Vec3d v3d = geo_to_vec3d(g);
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
  CoordIJK i_vec{3, 0, 1}, j_vec{1, 3, 0}, k_vec{0, 1, 3};
  ijk_scale(i_vec, ijk.i);
  ijk_scale(j_vec, ijk.j);
  ijk_scale(k_vec, ijk.k);
  ijk_add(i_vec, j_vec, ijk);
  ijk_add(ijk, k_vec, ijk);
  ijk_normalize(ijk);
}
static inline void down_ap7r(CoordIJK& ijk) {
  CoordIJK i_vec{3, 1, 0}, j_vec{0, 3, 1}, k_vec{1, 0, 3};
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
  return base_cell >= 0 && base_cell < NUM_BASE_CELLS && baseCellData[base_cell].isPentagon;
}
static inline bool base_cell_is_cw_offset(int base_cell, int test_face) {
  return baseCellData[base_cell].cwOffsetPent[0] == test_face ||
         baseCellData[base_cell].cwOffsetPent[1] == test_face;
}
static inline int face_ijk_to_base_cell(const FaceIJK& h) {
  return faceIjkBaseCells[h.face][h.coord.i][h.coord.j][h.coord.k].baseCell;
}
static inline int face_ijk_to_base_cell_ccw_rot60(const FaceIJK& h) {
  return faceIjkBaseCells[h.face][h.coord.i][h.coord.j][h.coord.k].ccwRot60;
}

static inline void geo_to_hex2d(const LatLng& g, int res, int& face, Vec2d& v) {
  double sqd;
  geo_to_closest_face(g, face, sqd);
  double r = std::acos(1.0 - sqd * 0.5);
  if (r < EPSILON) {
    v.x = 0.0;
    v.y = 0.0;
    return;
  }
  double theta = pos_angle_rads(faceAxesAzRadsCII[face][0] -
                                pos_angle_rads(geo_azimuth_rads(faceCenterGeo[face], g)));
  if (is_resolution_class_iii(res))
    theta = pos_angle_rads(theta - M_AP7_ROT_RADS);
  r = std::tan(r);
  r *= INV_RES0_U_GNOMONIC;
  for (int i = 0; i < res; i++)
    r *= M_SQRT7;
  v.x = r * std::cos(theta);
  v.y = r * std::sin(theta);
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
  if (res < 0 || res > MAX_H3_RES)
    return {0, 0};
  if (!std::isfinite(lat_deg) || !std::isfinite(lng_deg))
    return {0, 0};
  if (lat_deg < -90.0 || lat_deg > 90.0 || lng_deg < -180.0 || lng_deg > 180.0)
    return {0, 0};
  LatLng g{lat_deg * M_PI_180, lng_deg * M_PI_180};
  FaceIJK fijk = geo_to_face_ijk(g, res);
  H3Index out = face_ijk_to_h3(fijk, res);
  return {out, out == 0 ? uint8_t(0) : uint8_t(1)};
}

}  // namespace pgaccel_h3_exact

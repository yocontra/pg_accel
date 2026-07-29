#include <sys/wait.h>
#include <unistd.h>

#include <algorithm>
#include <cerrno>
#include <cmath>
#include <cstdio>
#include <cstdlib>
#include <cstring>
#include <iterator>
#include <limits>
#include <stdexcept>
#include <unordered_map>
#include <vector>

#include "pgaccel_expr.h"
#include "pgaccel_ffi.h"
#include "pgaccel_resident_count.h"

static int g_pass = 0;
static int g_fail = 0;

static_assert(PGACCEL_H3_PARENT_DETAIL_NONE == 0);
static_assert(PGACCEL_H3_PARENT_DETAIL_CONTRACT == 1);
static_assert(PGACCEL_H3_PARENT_DETAIL_INVALID_CELL == 2);
static_assert(PGACCEL_H3_PARENT_DETAIL_RES_MISMATCH == 3);

template <typename T>
class SharedResidentArray {
 public:
  explicit SharedResidentArray(const std::vector<T>& values) : count_(values.size()) {
    if (count_ == 0)
      return;
    void* allocation = nullptr;
    if (pgaccel_expr_shared_alloc(count_ * sizeof(T), &allocation) != PGACCEL_OK ||
        allocation == nullptr)
      throw std::runtime_error("shared resident allocation failed");
    values_ = static_cast<T*>(allocation);
    std::copy(values.begin(), values.end(), values_);
  }

  SharedResidentArray(const SharedResidentArray&) = delete;
  SharedResidentArray& operator=(const SharedResidentArray&) = delete;
  ~SharedResidentArray() { pgaccel_expr_shared_free(values_); }

  T* data() { return values_; }
  const T* data() const { return values_; }
  T& operator[](size_t index) { return values_[index]; }
  const T& operator[](size_t index) const { return values_[index]; }
  size_t size() const { return count_; }

 private:
  T* values_ = nullptr;
  size_t count_ = 0;
};

class DeviceResidentAllocation {
 public:
  DeviceResidentAllocation(const void* initial, size_t bytes) : bytes_(bytes) {
    const pgaccel_status status = initial == nullptr
                                      ? pgaccel_expr_device_alloc(bytes, &pointer_)
                                      : pgaccel_expr_device_alloc_copy(initial, bytes, &pointer_);
    if (status != PGACCEL_OK || (bytes != 0 && pointer_ == nullptr))
      throw std::runtime_error("device resident allocation failed");
  }

  DeviceResidentAllocation(const DeviceResidentAllocation&) = delete;
  DeviceResidentAllocation& operator=(const DeviceResidentAllocation&) = delete;
  ~DeviceResidentAllocation() { pgaccel_expr_device_free(pointer_); }

  void* data() { return pointer_; }
  const void* data() const { return pointer_; }
  size_t bytes() const { return bytes_; }

 private:
  void* pointer_ = nullptr;
  size_t bytes_ = 0;
};

#define ASSERT_EQ(desc, actual, expected)                                                    \
  do {                                                                                       \
    if ((actual) == (expected)) {                                                            \
      g_pass++;                                                                              \
    } else {                                                                                 \
      fprintf(stderr, "FAIL: %s — expected %lld, got %lld\n", (desc), (long long)(expected), \
              (long long)(actual));                                                          \
      g_fail++;                                                                              \
    }                                                                                        \
  } while (0)

#define ASSERT_STATUS_OK(desc, status)                                                \
  do {                                                                                \
    if ((status) == PGACCEL_OK) {                                                     \
      g_pass++;                                                                       \
    } else {                                                                          \
      fprintf(stderr, "FAIL: %s — status %d (expected OK)\n", (desc), (int)(status)); \
      g_fail++;                                                                       \
    }                                                                                 \
  } while (0)

#define ASSERT_TRUE(desc, cond)              \
  do {                                       \
    if ((cond)) {                            \
      g_pass++;                              \
    } else {                                 \
      fprintf(stderr, "FAIL: %s\n", (desc)); \
      g_fail++;                              \
    }                                        \
  } while (0)

// ---------------------------------------------------------------------------
// Helper: build a known H3 cell ID manually for testing
// ---------------------------------------------------------------------------
// Cell ID layout:
//   bit 63       = 0 (reserved)
//   bits 62-59   = mode (1 for cell)
//   bits 58-56   = reserved (0)
//   bits 55-52   = resolution
//   bits 51-45   = base cell
//   bits 44-0    = 15 x 3-bit digits (unused = 7)
static uint64_t make_cell(int base_cell, int resolution, const int* digits) {
  uint64_t cell = 0;
  cell |= (1ULL << 59);  // mode = 1
  cell |= ((uint64_t)(resolution & 0xF) << 52);
  cell |= ((uint64_t)(base_cell & 0x7F) << 45);
  // H3 v4 layout: digit r ∈ [1..15] at bits [(15-r)*3+2 .. (15-r)*3].
  // No `+1` reserved-bit offset (older revisions of this helper had one,
  // which silently corrupted base-cell read-back via bit-45 overlap).
  for (int r = 1; r <= 15; r++) {
    int shift = (15 - r) * 3;
    if (r <= resolution) {
      cell |= ((uint64_t)(digits[r - 1] & 0x7) << shift);
    } else {
      cell |= (7ULL << shift);
    }
  }
  return cell;
}

static int h3_cell_mode(uint64_t cell) {
  return static_cast<int>((cell >> 59) & 0xFULL);
}

static int h3_cell_base(uint64_t cell) {
  return static_cast<int>((cell >> 45) & 0x7FULL);
}

static double h3_rad_to_deg(double radians) {
  return radians * 57.295779513082320876;
}

static double h3_deg_to_rad(double degrees) {
  return degrees * 0.01745329251994329577;
}

static void h3_lat_lng_to_unit(double lat_deg, double lng_deg, double& x, double& y, double& z) {
  const double lat = h3_deg_to_rad(lat_deg);
  const double lng = h3_deg_to_rad(lng_deg);
  const double c = std::cos(lat);
  x = c * std::cos(lng);
  y = c * std::sin(lng);
  z = std::sin(lat);
}

static void h3_add_face_midpoint(std::vector<double>& lats, std::vector<double>& lngs, double lat_a,
                                 double lng_a, double lat_b, double lng_b) {
  double ax, ay, az, bx, by, bz;
  h3_lat_lng_to_unit(lat_a, lng_a, ax, ay, az);
  h3_lat_lng_to_unit(lat_b, lng_b, bx, by, bz);
  double mx = ax + bx;
  double my = ay + by;
  double mz = az + bz;
  const double inv_norm = 1.0 / std::sqrt(mx * mx + my * my + mz * mz);
  mx *= inv_norm;
  my *= inv_norm;
  mz *= inv_norm;
  lats.push_back(h3_rad_to_deg(std::asin(mz)));
  lngs.push_back(h3_rad_to_deg(std::atan2(my, mx)));
}

static void h3_add_deterministic_random_points(std::vector<double>& lats, std::vector<double>& lngs,
                                               size_t count) {
  uint64_t state = 0x9e3779b97f4a7c15ULL;
  for (size_t i = 0; i < count; ++i) {
    state = state * 6364136223846793005ULL + 1442695040888963407ULL;
    const double u =
        static_cast<double>((state >> 11) & ((1ULL << 53) - 1)) / static_cast<double>(1ULL << 53);
    state = state * 6364136223846793005ULL + 1442695040888963407ULL;
    const double v =
        static_cast<double>((state >> 11) & ((1ULL << 53) - 1)) / static_cast<double>(1ULL << 53);
    lats.push_back(-85.0 + u * 170.0);
    lngs.push_back(-179.5 + v * 359.0);
  }
}

static void h3_add_edge_coverage_points(std::vector<double>& lats, std::vector<double>& lngs) {
  const double fixed[][2] = {
      // Equator and prime-meridian axes.
      {0.0, 0.0},
      {0.0, 90.0},
      {0.0, -90.0},
      {0.000001, 179.999999},
      {-0.000001, -179.999999},
      // Near-pole inputs remain inside the valid coordinate range.
      {89.999999, 0.0},
      {89.999999, 179.999999},
      {89.999999, -179.999999},
      {-89.999999, 0.0},
      {-89.999999, 179.999999},
      {-89.999999, -179.999999},
      // Antimeridian-adjacent rows at mixed latitudes.
      {45.0, 179.999999},
      {45.0, -179.999999},
      {-45.0, 179.999999},
      {-45.0, -179.999999},
      {10.0, 179.999},
      {-10.0, -179.999},
  };
  for (const auto& p : fixed) {
    lats.push_back(p[0]);
    lngs.push_back(p[1]);
  }

  // H3 icosahedron face centers from the in-repo exact-device implementation.
  const double face_centers_rad[20][2] = {
      {0.803582649718989942, 1.248397419617396099},
      {1.307747883455638156, 2.536945009877921159},
      {1.054751253523952054, -1.347517358900396623},
      {0.600191595538186799, -0.450603909469755746},
      {0.491715428198773866, 0.401988202911306943},
      {0.172745327415618701, 1.678146885280433686},
      {0.605929321571350690, 2.953923329812411617},
      {0.427370518328979641, -1.888876200336285401},
      {-0.079066118549212831, -0.733429513380867741},
      {-0.230961644455383637, 0.506495587332349035},
      {0.079066118549212831, 2.408163140208925497},
      {0.230961644455383637, -2.635097066257444203},
      {-0.172745327415618701, -1.463445768309359553},
      {-0.605929321571350690, -0.187669323777381622},
      {-0.427370518328979641, 1.252716453253507838},
      {-0.600191595538186799, 2.690988744120037492},
      {-0.491715428198773866, -2.739604450678486295},
      {-0.803582649718989942, -1.893195233972397139},
      {-1.307747883455638156, -0.604647643711872080},
      {-1.054751253523952054, 1.794075294689396615},
  };

  double face_lats[20];
  double face_lngs[20];
  double face_x[20], face_y[20], face_z[20];
  for (int i = 0; i < 20; ++i) {
    face_lats[i] = h3_rad_to_deg(face_centers_rad[i][0]);
    face_lngs[i] = h3_rad_to_deg(face_centers_rad[i][1]);
    lats.push_back(face_lats[i]);
    lngs.push_back(face_lngs[i]);
    h3_lat_lng_to_unit(face_lats[i], face_lngs[i], face_x[i], face_y[i], face_z[i]);
  }

  // Adjacent face centers have dot product around 0.745; their normalized
  // midpoint lies on the spherical face-edge bisector.
  for (int a = 0; a < 20; ++a) {
    for (int b = a + 1; b < 20; ++b) {
      const double dot = face_x[a] * face_x[b] + face_y[a] * face_y[b] + face_z[a] * face_z[b];
      if (dot > 0.70) {
        h3_add_face_midpoint(lats, lngs, face_lats[a], face_lngs[a], face_lats[b], face_lngs[b]);
      }
    }
  }
}

// ---------------------------------------------------------------------------
// Test: get_resolution
// ---------------------------------------------------------------------------
static void test_get_resolution() {
  printf("--- test_get_resolution ---\n");

  // Build cells at various resolutions
  int digits[15] = {0};
  for (int res = 0; res <= 15; res++) {
    uint64_t cell = make_cell(0, res, digits);
    int32_t result = -1;
    pgaccel_status s = pgaccel_h3_get_resolution_bulk(&cell, 1, &result);
    ASSERT_STATUS_OK("get_resolution status", s);
    ASSERT_EQ("resolution matches", result, res);
  }

  // Bulk operation
  const size_t N = 4;
  uint64_t cells[N];
  int32_t results[N];
  cells[0] = make_cell(5, 3, digits);
  cells[1] = make_cell(10, 7, digits);
  cells[2] = make_cell(100, 15, digits);
  cells[3] = make_cell(0, 0, digits);

  pgaccel_status s = pgaccel_h3_get_resolution_bulk(cells, N, results);
  ASSERT_STATUS_OK("bulk get_resolution status", s);
  ASSERT_EQ("bulk res[0]", results[0], 3);
  ASSERT_EQ("bulk res[1]", results[1], 7);
  ASSERT_EQ("bulk res[2]", results[2], 15);
  ASSERT_EQ("bulk res[3]", results[3], 0);

  // Invalid cell (0) should return -1
  uint64_t zero = 0;
  int32_t zero_res = 99;
  s = pgaccel_h3_get_resolution_bulk(&zero, 1, &zero_res);
  ASSERT_STATUS_OK("zero cell status", s);
  ASSERT_EQ("zero cell res", zero_res, -1);

  // Empty count
  s = pgaccel_h3_get_resolution_bulk(nullptr, 0, nullptr);
  ASSERT_STATUS_OK("empty count status", s);
}

// ---------------------------------------------------------------------------
// Test: cell_to_parent
// ---------------------------------------------------------------------------
static void test_cell_to_parent() {
  printf("--- test_cell_to_parent ---\n");

  // Cell at res 5 with digits {1, 2, 3, 4, 5}
  int digits[15] = {1, 2, 3, 4, 5, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0};
  uint64_t cell = make_cell(10, 5, digits);

  // Parent at res 3 should keep digits {1, 2, 3} and set rest to 7
  int parent_digits[15] = {1, 2, 3, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0};
  uint64_t expected_parent = make_cell(10, 3, parent_digits);

  uint64_t parent = 0;
  pgaccel_status s = pgaccel_h3_cell_to_parent_bulk(&cell, 1, 3, &parent);
  ASSERT_STATUS_OK("cell_to_parent status", s);
  ASSERT_EQ("parent at res 3", parent, expected_parent);

  // Parent at res 0 — base cell only
  int base_digits[15] = {0};
  uint64_t expected_base = make_cell(10, 0, base_digits);
  s = pgaccel_h3_cell_to_parent_bulk(&cell, 1, 0, &parent);
  ASSERT_STATUS_OK("parent at res 0 status", s);
  ASSERT_EQ("parent at res 0", parent, expected_base);

  // Parent at same resolution = identity
  s = pgaccel_h3_cell_to_parent_bulk(&cell, 1, 5, &parent);
  ASSERT_STATUS_OK("parent at same res status", s);
  ASSERT_EQ("parent at same res", parent, cell);

  // Parent at higher resolution = invalid (0)
  s = pgaccel_h3_cell_to_parent_bulk(&cell, 1, 6, &parent);
  ASSERT_STATUS_OK("parent at higher res status", s);
  ASSERT_EQ("parent at higher res", parent, 0ULL);

  // Invalid parent_res
  s = pgaccel_h3_cell_to_parent_bulk(&cell, 1, -1, &parent);
  ASSERT_EQ("negative res returns UNSUPPORTED", s, PGACCEL_ERROR_UNSUPPORTED);

  s = pgaccel_h3_cell_to_parent_bulk(&cell, 1, 16, &parent);
  ASSERT_EQ("res 16 returns UNSUPPORTED", s, PGACCEL_ERROR_UNSUPPORTED);

  // Zero cell
  uint64_t zero = 0;
  s = pgaccel_h3_cell_to_parent_bulk(&zero, 1, 0, &parent);
  ASSERT_STATUS_OK("zero cell status", s);
  ASSERT_EQ("zero cell parent", parent, 0ULL);

  // Bulk
  const size_t N = 3;
  uint64_t cells[N];
  uint64_t parents[N];
  int d0[15] = {1, 2, 3, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0};
  int d1[15] = {4, 5, 6, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0};
  int d2[15] = {0, 1, 2, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0};
  cells[0] = make_cell(5, 3, d0);
  cells[1] = make_cell(5, 3, d1);
  cells[2] = make_cell(5, 3, d2);
  s = pgaccel_h3_cell_to_parent_bulk(cells, N, 1, parents);
  ASSERT_STATUS_OK("bulk parent status", s);
  // All should share same base cell and res-1 structure
  int p0[15] = {1, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0};
  int p1[15] = {4, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0};
  int p2[15] = {0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0};
  ASSERT_EQ("bulk parent[0]", parents[0], make_cell(5, 1, p0));
  ASSERT_EQ("bulk parent[1]", parents[1], make_cell(5, 1, p1));
  ASSERT_EQ("bulk parent[2]", parents[2], make_cell(5, 1, p2));

  ASSERT_STATUS_OK("empty parent input", pgaccel_h3_cell_to_parent_bulk(nullptr, 0, 0, nullptr));
}

static void test_cell_to_parent_resident() {
  printf("--- test_cell_to_parent_resident ---\n");
  constexpr uint64_t sentinel = UINT64_C(0xfedcba9876543210);
  int digits_a[15] = {2, 3, 4, 5, 6, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0};
  int digits_b[15] = {6, 5, 4, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0};
  const uint64_t cell_a = make_cell(10, 5, digits_a);
  const uint64_t cell_b = make_cell(20, 3, digits_b);

  std::vector<uint64_t> input = {cell_a, cell_b, 0, cell_a | (UINT64_C(1) << 63)};
  std::vector<uint8_t> input_nulls = {0, 0, 1, 1};
  SharedResidentArray<uint64_t> cells(input);
  SharedResidentArray<uint8_t> nulls(input_nulls);
  SharedResidentArray<uint64_t> parents(std::vector<uint64_t>(input.size(), sentinel));
  uint64_t expected_a = 0;
  ASSERT_STATUS_OK("resident oracle parent status",
                   pgaccel_h3_cell_to_parent_bulk(&cell_a, 1, 3, &expected_a));

  pgaccel_reset_gpu_exec_count();
  pgaccel_status status = pgaccel_h3_cell_to_parent_resident(cells.data(), nulls.data(),
                                                             cells.size(), 3, parents.data());
  ASSERT_STATUS_OK("resident shared parent status", status);
  ASSERT_TRUE("resident parent dispatched", pgaccel_gpu_exec_count() > 0);
  ASSERT_EQ("resident parent transformed", parents[0], expected_a);
  ASSERT_EQ("resident parent identity", parents[1], cell_b);
  ASSERT_EQ("resident null zero[0]", parents[2], 0ULL);
  ASSERT_EQ("resident null zero[1]", parents[3], 0ULL);
  ASSERT_TRUE("resident input values unchanged",
              std::equal(input.begin(), input.end(), cells.data()));
  ASSERT_TRUE("resident null sidecar unchanged",
              std::equal(input_nulls.begin(), input_nulls.end(), nulls.data()));

  int32_t detail = PGACCEL_H3_PARENT_DETAIL_CONTRACT;
  status = pgaccel_h3_cell_to_parent_resident_ex(cells.data(), nulls.data(), cells.size(), 3,
                                                 parents.data(), &detail);
  ASSERT_STATUS_OK("resident ex success status", status);
  ASSERT_EQ("resident ex success detail", detail, PGACCEL_H3_PARENT_DETAIL_NONE);

  std::vector<uint64_t> device_input = {cell_a, cell_b, cell_a};
  int high_base_digits[15] = {0};
  const uint64_t high_base_pentagon = make_cell(72, 3, high_base_digits);
  device_input.push_back(high_base_pentagon);
  std::vector<uint64_t> device_expected(device_input.size(), 0);
  ASSERT_STATUS_OK("device resident oracle status",
                   pgaccel_h3_cell_to_parent_bulk(device_input.data(), device_input.size(), 2,
                                                  device_expected.data()));
  DeviceResidentAllocation device_cells(device_input.data(),
                                        device_input.size() * sizeof(uint64_t));
  DeviceResidentAllocation device_parents(nullptr, device_input.size() * sizeof(uint64_t));
  std::vector<uint64_t> device_initial(device_input.size(), sentinel);
  ASSERT_STATUS_OK("device resident output initialize",
                   pgaccel_expr_device_copy_from_host(device_parents.data(), device_initial.data(),
                                                      device_parents.bytes()));
  status = pgaccel_h3_cell_to_parent_resident(static_cast<const uint64_t*>(device_cells.data()),
                                              nullptr, device_input.size(), 2,
                                              static_cast<uint64_t*>(device_parents.data()));
  ASSERT_STATUS_OK("resident device parent status", status);
  std::vector<uint64_t> device_actual(device_input.size(), 0);
  ASSERT_STATUS_OK("device resident output read",
                   pgaccel_expr_device_copy_to_host(device_actual.data(), device_parents.data(),
                                                    device_parents.bytes()));
  ASSERT_TRUE("resident device output matches oracle", device_actual == device_expected);

  const uint64_t reserved_bits = cell_a | (UINT64_C(1) << 56);
  const uint64_t wrong_mode = (cell_a & ~(UINT64_C(0xf) << 59)) | (UINT64_C(2) << 59);
  const uint64_t invalid_base = (cell_a & ~(UINT64_C(0x7f) << 45)) | (UINT64_C(122) << 45);
  int invalid_digit_values[15] = {7, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0};
  const uint64_t invalid_used_digit = make_cell(10, 5, invalid_digit_values);
  const uint64_t invalid_unused_digit = cell_a & ~(UINT64_C(7) << ((15 - 6) * 3));
  int deleted_pentagon_values[15] = {1, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0};
  const uint64_t deleted_pentagon = make_cell(4, 1, deleted_pentagon_values);
  int coarse_values[15] = {2, 3, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0};
  const uint64_t too_coarse = make_cell(10, 2, coarse_values);
  std::vector<uint64_t> malformed = {0,
                                     cell_a | (UINT64_C(1) << 63),
                                     reserved_bits,
                                     wrong_mode,
                                     invalid_base,
                                     invalid_used_digit,
                                     invalid_unused_digit,
                                     deleted_pentagon,
                                     too_coarse};
  SharedResidentArray<uint64_t> malformed_cells(malformed);
  SharedResidentArray<uint64_t> rejected_output(std::vector<uint64_t>(malformed.size(), sentinel));
  status = pgaccel_h3_cell_to_parent_resident(malformed_cells.data(), nullptr, malformed.size(), 3,
                                              rejected_output.data());
  ASSERT_EQ("resident malformed cells fail", status, PGACCEL_INVALID_ARGUMENT);
  bool rejected_unchanged = true;
  for (size_t i = 0; i < malformed.size(); ++i)
    rejected_unchanged = rejected_unchanged && rejected_output[i] == sentinel;
  ASSERT_TRUE("resident malformed call publishes nothing", rejected_unchanged);

  detail = PGACCEL_H3_PARENT_DETAIL_NONE;
  status = pgaccel_h3_cell_to_parent_resident_ex(
      malformed_cells.data(), nullptr, malformed.size() - 1, 3, rejected_output.data(), &detail);
  ASSERT_EQ("resident ex invalid cell status", status, PGACCEL_INVALID_ARGUMENT);
  ASSERT_EQ("resident ex invalid cell detail", detail, PGACCEL_H3_PARENT_DETAIL_INVALID_CELL);

  SharedResidentArray<uint64_t> coarse_cell(std::vector<uint64_t>({too_coarse}));
  SharedResidentArray<uint64_t> coarse_output(std::vector<uint64_t>({sentinel}));
  detail = PGACCEL_H3_PARENT_DETAIL_NONE;
  status = pgaccel_h3_cell_to_parent_resident_ex(coarse_cell.data(), nullptr, coarse_cell.size(), 3,
                                                 coarse_output.data(), &detail);
  ASSERT_EQ("resident ex resolution mismatch status", status, PGACCEL_INVALID_ARGUMENT);
  ASSERT_EQ("resident ex resolution mismatch detail", detail,
            PGACCEL_H3_PARENT_DETAIL_RES_MISMATCH);
  ASSERT_EQ("resident ex resolution mismatch publishes nothing", coarse_output[0], sentinel);

  SharedResidentArray<uint64_t> invalid_and_coarse_cells(
      std::vector<uint64_t>({wrong_mode, too_coarse}));
  SharedResidentArray<uint64_t> invalid_and_coarse_output(std::vector<uint64_t>(2, sentinel));
  detail = PGACCEL_H3_PARENT_DETAIL_NONE;
  status = pgaccel_h3_cell_to_parent_resident_ex(invalid_and_coarse_cells.data(), nullptr,
                                                 invalid_and_coarse_cells.size(), 3,
                                                 invalid_and_coarse_output.data(), &detail);
  ASSERT_EQ("resident ex mixed invalid cell status", status, PGACCEL_INVALID_ARGUMENT);
  ASSERT_EQ("resident ex mixed invalid cell precedence", detail,
            PGACCEL_H3_PARENT_DETAIL_INVALID_CELL);

  SharedResidentArray<uint8_t> malformed_nulls(std::vector<uint8_t>({0, 2, 0, 0}));
  SharedResidentArray<uint64_t> malformed_null_output(
      std::vector<uint64_t>(input.size(), sentinel));
  status = pgaccel_h3_cell_to_parent_resident(cells.data(), malformed_nulls.data(), cells.size(), 3,
                                              malformed_null_output.data());
  ASSERT_EQ("resident malformed null sidecar fails", status, PGACCEL_INVALID_ARGUMENT);
  ASSERT_TRUE("resident malformed null publishes nothing",
              std::all_of(malformed_null_output.data(),
                          malformed_null_output.data() + malformed_null_output.size(),
                          [=](uint64_t value) { return value == sentinel; }));

  detail = PGACCEL_H3_PARENT_DETAIL_NONE;
  status = pgaccel_h3_cell_to_parent_resident_ex(cells.data(), malformed_nulls.data(), cells.size(),
                                                 3, malformed_null_output.data(), &detail);
  ASSERT_EQ("resident ex malformed null status", status, PGACCEL_INVALID_ARGUMENT);
  ASSERT_EQ("resident ex malformed null detail", detail, PGACCEL_H3_PARENT_DETAIL_CONTRACT);

  SharedResidentArray<uint64_t> mixed_cells(std::vector<uint64_t>({too_coarse, 0}));
  SharedResidentArray<uint8_t> mixed_nulls(std::vector<uint8_t>({0, 2}));
  SharedResidentArray<uint64_t> mixed_output(std::vector<uint64_t>(2, sentinel));
  detail = PGACCEL_H3_PARENT_DETAIL_NONE;
  status = pgaccel_h3_cell_to_parent_resident_ex(
      mixed_cells.data(), mixed_nulls.data(), mixed_cells.size(), 3, mixed_output.data(), &detail);
  ASSERT_EQ("resident ex mixed contract status", status, PGACCEL_INVALID_ARGUMENT);
  ASSERT_EQ("resident ex mixed contract precedence", detail, PGACCEL_H3_PARENT_DETAIL_CONTRACT);

  SharedResidentArray<uint64_t> null_only_cells(
      std::vector<uint64_t>({0, UINT64_C(0xffffffffffffffff)}));
  SharedResidentArray<uint8_t> null_only_sidecar(std::vector<uint8_t>({1, 1}));
  SharedResidentArray<uint64_t> null_only_output(
      std::vector<uint64_t>(null_only_cells.size(), sentinel));
  status = pgaccel_h3_cell_to_parent_resident(null_only_cells.data(), null_only_sidecar.data(),
                                              null_only_cells.size(), 15, null_only_output.data());
  ASSERT_STATUS_OK("resident null rows skip cell validation", status);
  ASSERT_EQ("resident all-null output[0]", null_only_output[0], 0ULL);
  ASSERT_EQ("resident all-null output[1]", null_only_output[1], 0ULL);

  ASSERT_EQ("resident invalid resolution fails",
            pgaccel_h3_cell_to_parent_resident(cells.data(), nulls.data(), cells.size(), 16,
                                               parents.data()),
            PGACCEL_INVALID_ARGUMENT);
  ASSERT_EQ(
      "resident aliased value/output fails",
      pgaccel_h3_cell_to_parent_resident(cells.data(), nulls.data(), cells.size(), 3, cells.data()),
      PGACCEL_INVALID_ARGUMENT);
  detail = PGACCEL_H3_PARENT_DETAIL_NONE;
  ASSERT_EQ("resident ex aliased value/output fails",
            pgaccel_h3_cell_to_parent_resident_ex(cells.data(), nulls.data(), cells.size(), 3,
                                                  cells.data(), &detail),
            PGACCEL_INVALID_ARGUMENT);
  ASSERT_EQ("resident ex aliased value/output detail", detail, PGACCEL_H3_PARENT_DETAIL_CONTRACT);
  ASSERT_EQ("resident host pointers fail",
            pgaccel_h3_cell_to_parent_resident(input.data(), input_nulls.data(), input.size(), 3,
                                               device_expected.data()),
            PGACCEL_INVALID_ARGUMENT);
  detail = PGACCEL_H3_PARENT_DETAIL_NONE;
  ASSERT_EQ("resident ex host pointers fail",
            pgaccel_h3_cell_to_parent_resident_ex(input.data(), input_nulls.data(), input.size(), 3,
                                                  device_expected.data(), &detail),
            PGACCEL_INVALID_ARGUMENT);
  ASSERT_EQ("resident ex host pointer detail", detail, PGACCEL_H3_PARENT_DETAIL_CONTRACT);

  uint64_t contract_cell = input[0];
  uint64_t contract_parent = sentinel;
  detail = PGACCEL_H3_PARENT_DETAIL_NONE;
  ASSERT_EQ(
      "resident ex null cells fail",
      pgaccel_h3_cell_to_parent_resident_ex(nullptr, nullptr, 1, 3, &contract_parent, &detail),
      PGACCEL_INVALID_ARGUMENT);
  ASSERT_EQ("resident ex null cells detail", detail, PGACCEL_H3_PARENT_DETAIL_CONTRACT);
  detail = PGACCEL_H3_PARENT_DETAIL_NONE;
  ASSERT_EQ("resident ex null parents fail",
            pgaccel_h3_cell_to_parent_resident_ex(&contract_cell, nullptr, 1, 3, nullptr, &detail),
            PGACCEL_INVALID_ARGUMENT);
  ASSERT_EQ("resident ex null parents detail", detail, PGACCEL_H3_PARENT_DETAIL_CONTRACT);

  const size_t span_overflow_count =
      std::numeric_limits<size_t>::max() / sizeof(uint64_t) + size_t{1};
  detail = PGACCEL_H3_PARENT_DETAIL_NONE;
  ASSERT_EQ("resident ex count span overflow fails",
            pgaccel_h3_cell_to_parent_resident_ex(&contract_cell, nullptr, span_overflow_count, 3,
                                                  &contract_parent, &detail),
            PGACCEL_INVALID_ARGUMENT);
  ASSERT_EQ("resident ex count span overflow detail", detail, PGACCEL_H3_PARENT_DETAIL_CONTRACT);

  const uintptr_t near_address_limit =
      std::numeric_limits<uintptr_t>::max() - sizeof(uint64_t) + uintptr_t{2};
  const auto* overflowing_cells = reinterpret_cast<const uint64_t*>(near_address_limit);
  detail = PGACCEL_H3_PARENT_DETAIL_NONE;
  ASSERT_EQ("resident ex address span overflow fails",
            pgaccel_h3_cell_to_parent_resident_ex(overflowing_cells, nullptr, 1, 3,
                                                  &contract_parent, &detail),
            PGACCEL_INVALID_ARGUMENT);
  ASSERT_EQ("resident ex address span overflow detail", detail, PGACCEL_H3_PARENT_DETAIL_CONTRACT);

  ASSERT_STATUS_OK("resident empty input",
                   pgaccel_h3_cell_to_parent_resident(nullptr, nullptr, 0, 3, nullptr));
  detail = PGACCEL_H3_PARENT_DETAIL_CONTRACT;
  ASSERT_STATUS_OK("resident ex empty input",
                   pgaccel_h3_cell_to_parent_resident_ex(nullptr, nullptr, 0, 3, nullptr, &detail));
  ASSERT_EQ("resident ex empty detail", detail, PGACCEL_H3_PARENT_DETAIL_NONE);
  ASSERT_EQ("resident ex null detail fails",
            pgaccel_h3_cell_to_parent_resident_ex(nullptr, nullptr, 0, 3, nullptr, nullptr),
            PGACCEL_INVALID_ARGUMENT);
  ASSERT_EQ("resident empty still validates resolution",
            pgaccel_h3_cell_to_parent_resident(nullptr, nullptr, 0, -1, nullptr),
            PGACCEL_INVALID_ARGUMENT);
  detail = PGACCEL_H3_PARENT_DETAIL_NONE;
  ASSERT_EQ("resident ex invalid resolution status",
            pgaccel_h3_cell_to_parent_resident_ex(nullptr, nullptr, 0, -1, nullptr, &detail),
            PGACCEL_INVALID_ARGUMENT);
  ASSERT_EQ("resident ex invalid resolution detail", detail, PGACCEL_H3_PARENT_DETAIL_CONTRACT);
}

// ---------------------------------------------------------------------------
// Test: grid_distance
// ---------------------------------------------------------------------------
static void test_grid_distance() {
  printf("--- test_grid_distance ---\n");

  // Same cell -> distance 0
  int digits[15] = {1, 2, 3, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0};
  uint64_t cell = make_cell(5, 3, digits);
  int32_t dist = -99;
  pgaccel_status s = pgaccel_h3_grid_distance_bulk(&cell, &cell, 1, &dist);
  ASSERT_STATUS_OK("same cell distance status", s);
  ASSERT_EQ("same cell distance", dist, 0);

  // Different resolutions -> -1
  int d1[15] = {1, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0};
  uint64_t cell_r1 = make_cell(5, 1, d1);
  uint64_t cell_r3 = make_cell(5, 3, digits);
  s = pgaccel_h3_grid_distance_bulk(&cell_r1, &cell_r3, 1, &dist);
  ASSERT_STATUS_OK("diff res status", s);
  ASSERT_EQ("diff res distance", dist, -1);

  // Different base cells -> -1
  uint64_t cell_b5 = make_cell(5, 3, digits);
  uint64_t cell_b6 = make_cell(6, 3, digits);
  s = pgaccel_h3_grid_distance_bulk(&cell_b5, &cell_b6, 1, &dist);
  ASSERT_STATUS_OK("diff base status", s);
  ASSERT_EQ("diff base distance", dist, -1);

  // Zero cell -> -1
  uint64_t zero = 0;
  s = pgaccel_h3_grid_distance_bulk(&zero, &cell, 1, &dist);
  ASSERT_STATUS_OK("zero cell a status", s);
  ASSERT_EQ("zero cell a distance", dist, -1);

  s = pgaccel_h3_grid_distance_bulk(&cell, &zero, 1, &dist);
  ASSERT_STATUS_OK("zero cell b status", s);
  ASSERT_EQ("zero cell b distance", dist, -1);

  // Adjacent cells at res 1 in same base cell: digit 0 vs digit 1
  // Should produce some positive distance
  int da[15] = {0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0};
  int db[15] = {1, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0};
  uint64_t ca = make_cell(5, 1, da);
  uint64_t cb = make_cell(5, 1, db);
  s = pgaccel_h3_grid_distance_bulk(&ca, &cb, 1, &dist);
  ASSERT_STATUS_OK("adjacent cells status", s);
  ASSERT_TRUE("adjacent cells distance > 0", dist > 0);

  // Exercise every IJK direction and both subtraction orders in one device
  // dispatch. Besides checking symmetry, this covers each min/max
  // normalisation branch used by the same-base-cell distance kernel.
  std::vector<uint64_t> direction_cells;
  for (int digit = 0; digit <= 6; ++digit) {
    int direction_digits[15] = {digit, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0};
    direction_cells.push_back(make_cell(5, 1, direction_digits));
  }
  std::vector<uint64_t> direction_a;
  std::vector<uint64_t> direction_b;
  for (size_t a = 0; a < direction_cells.size(); ++a) {
    for (size_t b = 0; b < direction_cells.size(); ++b) {
      direction_a.push_back(direction_cells[a]);
      direction_b.push_back(direction_cells[b]);
    }
  }
  std::vector<int32_t> direction_distances(direction_a.size(), -99);
  pgaccel_reset_gpu_exec_count();
  s = pgaccel_h3_grid_distance_bulk(direction_a.data(), direction_b.data(), direction_a.size(),
                                    direction_distances.data());
  ASSERT_STATUS_OK("all-direction distance status", s);
  ASSERT_TRUE("all-direction distance dispatched", pgaccel_gpu_exec_count() > 0);
  bool direction_matrix_ok = true;
  for (size_t a = 0; a < direction_cells.size(); ++a) {
    for (size_t b = 0; b < direction_cells.size(); ++b) {
      const int32_t ab = direction_distances[a * direction_cells.size() + b];
      const int32_t ba = direction_distances[b * direction_cells.size() + a];
      direction_matrix_ok = direction_matrix_ok && ab >= 0 && ab == ba && (a != b || ab == 0);
    }
  }
  ASSERT_TRUE("all-direction distance matrix is symmetric", direction_matrix_ok);

  // The simplified kernel rejects malformed active digits defensively. Keep
  // this case in the batch so both malformed-input branches execute without
  // weakening the valid-cell assertions above.
  int invalid_digits[15] = {7, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0};
  const uint64_t invalid_digit_cell = make_cell(5, 1, invalid_digits);
  uint64_t malformed_a[2] = {invalid_digit_cell, ca};
  uint64_t malformed_b[2] = {ca, invalid_digit_cell};
  int32_t malformed_distance[2] = {-99, -99};
  s = pgaccel_h3_grid_distance_bulk(malformed_a, malformed_b, 2, malformed_distance);
  ASSERT_STATUS_OK("malformed active digit distance status", s);
  ASSERT_TRUE("malformed active digit distance is defined",
              malformed_distance[0] >= 0 && malformed_distance[1] >= 0);

  // Empty count
  s = pgaccel_h3_grid_distance_bulk(nullptr, nullptr, 0, nullptr);
  ASSERT_STATUS_OK("empty count status", s);
}

// ---------------------------------------------------------------------------
// Test: lat_lng_to_cell
// ---------------------------------------------------------------------------
static void test_lat_lng_to_cell() {
  printf("--- test_lat_lng_to_cell ---\n");

  // Basic: equator/prime meridian at res 0
  double lat = 0.0, lng = 0.0;
  uint64_t cell_id = 0;
  uint8_t valid = 0;
  pgaccel_status s = pgaccel_h3_lat_lng_to_cell_bulk(&lat, &lng, 1, 0, true, &cell_id, &valid);
  ASSERT_STATUS_OK("lat_lng_to_cell res 0 status", s);
  ASSERT_TRUE("lat_lng_to_cell res 0 valid", valid == 1);
  ASSERT_TRUE("lat_lng_to_cell res 0 non-zero", cell_id != 0);

  // Verify resolution of returned cell
  int32_t res_out = -1;
  pgaccel_h3_get_resolution_bulk(&cell_id, 1, &res_out);
  ASSERT_EQ("returned cell has correct resolution", res_out, 0);

  // Res 5 should also work
  s = pgaccel_h3_lat_lng_to_cell_bulk(&lat, &lng, 1, 5, true, &cell_id, &valid);
  ASSERT_STATUS_OK("lat_lng_to_cell res 5 status", s);
  ASSERT_TRUE("lat_lng_to_cell res 5 valid", valid == 1);
  pgaccel_h3_get_resolution_bulk(&cell_id, 1, &res_out);
  ASSERT_EQ("res 5 cell has correct resolution", res_out, 5);

  // Parent relationship: cell at res 5 -> parent at res 3 should match
  // a cell generated directly at res 3 (for the same lat/lng)
  // NOTE: This tests internal consistency, not H3 reference values.
  uint64_t cell_r5 = 0, cell_r3_direct = 0;
  uint8_t v5 = 0, v3 = 0;
  pgaccel_h3_lat_lng_to_cell_bulk(&lat, &lng, 1, 5, true, &cell_r5, &v5);
  pgaccel_h3_lat_lng_to_cell_bulk(&lat, &lng, 1, 3, true, &cell_r3_direct, &v3);
  if (v5 && v3) {
    uint64_t cell_r3_via_parent = 0;
    pgaccel_h3_cell_to_parent_bulk(&cell_r5, 1, 3, &cell_r3_via_parent);
    ASSERT_EQ("parent consistency", cell_r3_via_parent, cell_r3_direct);
  }

  // High-res requests stay SQL-correct even when the historical fp32 flag is
  // false: the H3 bulk path now computes a GPU candidate and exact-fixes
  // high-resolution cells rather than surfacing invalid rows to SQL.
  s = pgaccel_h3_lat_lng_to_cell_bulk(&lat, &lng, 1, 12, false, &cell_id, &valid);
  ASSERT_STATUS_OK("fp32 res 12 status", s);
  ASSERT_TRUE("fp32-flag res 12 valid", valid == 1);

  // fp64 at high res: post fp64-unlock (W1/W2/W3/W4), every backend
  // (including Metal via soft-fp64) must dispatch fp64 paths. An
  // UNSUPPORTED status here means the soft-fp64 lowering broke.
  pgaccel_reset_gpu_exec_count();
  uint64_t before = pgaccel_gpu_exec_count();
  s = pgaccel_h3_lat_lng_to_cell_bulk(&lat, &lng, 1, 12, true, &cell_id, &valid);
  ASSERT_STATUS_OK("fp64 res 12 status", s);
  ASSERT_TRUE("fp64 res 12 valid", valid == 1);
  uint64_t after = pgaccel_gpu_exec_count();
  ASSERT_TRUE("fp64 res 12 launched GPU kernels", after > before);

  // Invalid lat/lng
  double bad_lat = 100.0, bad_lng = 0.0;
  s = pgaccel_h3_lat_lng_to_cell_bulk(&bad_lat, &bad_lng, 1, 5, true, &cell_id, &valid);
  ASSERT_STATUS_OK("invalid lat status", s);
  ASSERT_EQ("invalid lat marked invalid", valid, 0);

  // Invalid resolution
  s = pgaccel_h3_lat_lng_to_cell_bulk(&lat, &lng, 1, 16, true, &cell_id, &valid);
  ASSERT_EQ("res 16 returns UNSUPPORTED", s, PGACCEL_ERROR_UNSUPPORTED);

  s = pgaccel_h3_lat_lng_to_cell_bulk(&lat, &lng, 1, -1, true, &cell_id, &valid);
  ASSERT_EQ("res -1 returns UNSUPPORTED", s, PGACCEL_ERROR_UNSUPPORTED);

  // Bulk: several well-known locations
  const size_t N = 5;
  double lats[N] = {40.689247, 48.858844, -33.856159, 35.659494, 51.500729};
  double lngs[N] = {-74.044502, 2.294351, 151.215256, 139.700472, -0.124625};
  uint64_t cells[N];
  uint8_t valids[N];
  s = pgaccel_h3_lat_lng_to_cell_bulk(lats, lngs, N, 4, true, cells, valids);
  ASSERT_STATUS_OK("bulk lat_lng status", s);
  int valid_count = 0;
  for (size_t i = 0; i < N; i++) {
    if (valids[i])
      valid_count++;
  }
  // Most points should be valid at res 4
  ASSERT_TRUE("most bulk points valid", valid_count >= 3);

  // All valid cells should have resolution 4
  for (size_t i = 0; i < N; i++) {
    if (valids[i]) {
      int32_t r = -1;
      pgaccel_h3_get_resolution_bulk(&cells[i], 1, &r);
      ASSERT_EQ("bulk cell has correct res", r, 4);
    }
  }

  // North pole
  double pole_lat = 90.0, pole_lng = 0.0;
  s = pgaccel_h3_lat_lng_to_cell_bulk(&pole_lat, &pole_lng, 1, 2, true, &cell_id, &valid);
  ASSERT_STATUS_OK("north pole status", s);
  // May or may not be valid depending on face edge detection — just check no crash

  // South pole
  pole_lat = -90.0;
  s = pgaccel_h3_lat_lng_to_cell_bulk(&pole_lat, &pole_lng, 1, 2, true, &cell_id, &valid);
  ASSERT_STATUS_OK("south pole status", s);

  // Empty count
  s = pgaccel_h3_lat_lng_to_cell_bulk(nullptr, nullptr, 0, 5, true, nullptr, nullptr);
  ASSERT_STATUS_OK("empty count status", s);
}

static void test_lat_lng_to_cell_bulk_edge_randomized() {
  printf("--- test_lat_lng_to_cell_bulk_edge_randomized ---\n");

  std::vector<double> lats;
  std::vector<double> lngs;
  h3_add_edge_coverage_points(lats, lngs);
  h3_add_deterministic_random_points(lats, lngs, 64);

  ASSERT_TRUE("edge/random point vectors aligned", lats.size() == lngs.size());
  ASSERT_TRUE("edge/random point set non-empty", !lats.empty());

  const size_t N = lats.size();
  std::vector<uint64_t> cells_a(N, 0), cells_b(N, 0);
  std::vector<uint8_t> valid_a(N, 0), valid_b(N, 0);
  std::vector<int32_t> resolutions(N, -1);

  for (int res = 0; res <= 15; ++res) {
    pgaccel_status s = pgaccel_h3_lat_lng_to_cell_bulk(
        lats.data(), lngs.data(), N, res, /*use_fp64=*/1, cells_a.data(), valid_a.data());
    char buf[128];
    snprintf(buf, sizeof(buf), "edge/random lat_lng_to_cell res=%d status", res);
    ASSERT_STATUS_OK(buf, s);

    s = pgaccel_h3_lat_lng_to_cell_bulk(lats.data(), lngs.data(), N, res, /*use_fp64=*/1,
                                        cells_b.data(), valid_b.data());
    snprintf(buf, sizeof(buf), "edge/random repeat res=%d status", res);
    ASSERT_STATUS_OK(buf, s);

    s = pgaccel_h3_get_resolution_bulk(cells_a.data(), N, resolutions.data());
    snprintf(buf, sizeof(buf), "edge/random get_resolution res=%d status", res);
    ASSERT_STATUS_OK(buf, s);

    bool all_valid = true;
    bool deterministic = true;
    bool cells_look_valid = true;
    bool res_ok = true;
    for (size_t i = 0; i < N; ++i) {
      if (valid_a[i] == 0 || cells_a[i] == 0) {
        all_valid = false;
      }
      if (valid_a[i] != valid_b[i] || cells_a[i] != cells_b[i]) {
        deterministic = false;
      }
      if (valid_a[i] != 0 && (h3_cell_mode(cells_a[i]) != 1 || h3_cell_base(cells_a[i]) > 121)) {
        cells_look_valid = false;
      }
      if (valid_a[i] != 0 && resolutions[i] != res) {
        res_ok = false;
      }
    }

    snprintf(buf, sizeof(buf), "edge/random res=%d all inputs valid", res);
    ASSERT_TRUE(buf, all_valid);
    snprintf(buf, sizeof(buf), "edge/random res=%d repeated outputs deterministic", res);
    ASSERT_TRUE(buf, deterministic);
    snprintf(buf, sizeof(buf), "edge/random res=%d valid-looking H3 cells", res);
    ASSERT_TRUE(buf, cells_look_valid);
    snprintf(buf, sizeof(buf), "edge/random res=%d resolution field matches", res);
    ASSERT_TRUE(buf, res_ok);
  }
}

static void test_lat_lng_to_cell_fp32_exact_matrix() {
  printf("--- test_lat_lng_to_cell_fp32_exact_matrix ---\n");

  std::vector<double> seed_lats;
  std::vector<double> seed_lngs;
  h3_add_edge_coverage_points(seed_lats, seed_lngs);
  h3_add_deterministic_random_points(seed_lats, seed_lngs, 2048);

  std::vector<float> lats(seed_lats.size());
  std::vector<float> lngs(seed_lngs.size());
  std::vector<double> exact_lats(seed_lats.size());
  std::vector<double> exact_lngs(seed_lngs.size());
  for (size_t i = 0; i < seed_lats.size(); ++i) {
    lats[i] = static_cast<float>(seed_lats[i]);
    lngs[i] = static_cast<float>(seed_lngs[i]);
    // The exact oracle must use the values representable by the caller's
    // fp32 input, rather than the pre-rounding doubles used to generate it.
    exact_lats[i] = static_cast<double>(lats[i]);
    exact_lngs[i] = static_cast<double>(lngs[i]);
  }

  const size_t count = lats.size();
  std::vector<uint64_t> fast_cells(count, 0);
  std::vector<uint64_t> exact_cells(count, 0);
  std::vector<uint8_t> fast_valid(count, 0);
  std::vector<uint8_t> exact_valid(count, 0);
  std::vector<int32_t> resolutions(count, -1);

  for (int resolution = 0; resolution < 12; ++resolution) {
    pgaccel_reset_gpu_exec_count();
    pgaccel_status status =
        pgaccel_h3_lat_lng_to_cell_bulk(lats.data(), lngs.data(), count, resolution, /*use_fp64=*/0,
                                        fast_cells.data(), fast_valid.data());
    char label[160];
    snprintf(label, sizeof(label), "fp32 public path res=%d status", resolution);
    ASSERT_STATUS_OK(label, status);
    snprintf(label, sizeof(label), "fp32 public path res=%d dispatched", resolution);
    ASSERT_TRUE(label, pgaccel_gpu_exec_count() > 0);

    status =
        pgaccel_h3_lat_lng_to_cell_bulk(exact_lats.data(), exact_lngs.data(), count, resolution,
                                        /*use_fp64=*/1, exact_cells.data(), exact_valid.data());
    snprintf(label, sizeof(label), "fp32 exact oracle res=%d status", resolution);
    ASSERT_STATUS_OK(label, status);

    bool exact_match = true;
    for (size_t i = 0; i < count; ++i) {
      exact_match = exact_match && fast_valid[i] == 1 && exact_valid[i] == 1 &&
                    fast_cells[i] != 0 && fast_cells[i] == exact_cells[i];
    }
    snprintf(label, sizeof(label), "fp32 public path res=%d matches rounded exact input",
             resolution);
    ASSERT_TRUE(label, exact_match);

    status = pgaccel_h3_get_resolution_bulk(fast_cells.data(), count, resolutions.data());
    snprintf(label, sizeof(label), "fp32 public path res=%d resolution status", resolution);
    ASSERT_STATUS_OK(label, status);
    snprintf(label, sizeof(label), "fp32 public path res=%d resolution fields", resolution);
    ASSERT_TRUE(label, std::all_of(resolutions.begin(), resolutions.end(),
                                   [=](int32_t value) { return value == resolution; }));
  }

  const float nan = std::numeric_limits<float>::quiet_NaN();
  const float infinity = std::numeric_limits<float>::infinity();
  const float invalid_lats[] = {90.001f, -90.001f, 0.0f, 0.0f, nan, 0.0f, infinity, -infinity};
  const float invalid_lngs[] = {0.0f, 0.0f, 180.001f, -180.001f, 0.0f, nan, 0.0f, 0.0f};
  constexpr size_t invalid_count = sizeof(invalid_lats) / sizeof(invalid_lats[0]);
  uint64_t invalid_cells[invalid_count];
  uint8_t invalid_valid[invalid_count];
  for (int resolution : {0, 7, 11}) {
    std::fill(std::begin(invalid_cells), std::end(invalid_cells), UINT64_MAX);
    std::fill(std::begin(invalid_valid), std::end(invalid_valid), uint8_t{99});
    pgaccel_status status =
        pgaccel_h3_lat_lng_to_cell_bulk(invalid_lats, invalid_lngs, invalid_count, resolution,
                                        /*use_fp64=*/0, invalid_cells, invalid_valid);
    char label[160];
    snprintf(label, sizeof(label), "fp32 invalid coordinates res=%d status", resolution);
    ASSERT_STATUS_OK(label, status);
    snprintf(label, sizeof(label), "fp32 invalid coordinates res=%d rejected", resolution);
    ASSERT_TRUE(label, std::all_of(std::begin(invalid_valid), std::end(invalid_valid),
                                   [](uint8_t value) { return value == 0; }) &&
                           std::all_of(std::begin(invalid_cells), std::end(invalid_cells),
                                       [](uint64_t value) { return value == 0; }));
  }
}

static void test_lat_lng_count_bulk() {
  printf("--- test_lat_lng_count_bulk ---\n");

  constexpr size_t UNIQUE_POINTS = 64;
  constexpr size_t N = 4096;
  constexpr int RESOLUTION = 7;
  double base_lats[UNIQUE_POINTS];
  double base_lngs[UNIQUE_POINTS];
  for (size_t i = 0; i < UNIQUE_POINTS; ++i) {
    base_lats[i] = -50.0 + static_cast<double>(i % 16) * 4.5;
    base_lngs[i] = -150.0 + static_cast<double>(i / 16) * 75.0;
  }

  std::vector<double> lats(N);
  std::vector<double> lngs(N);
  for (size_t i = 0; i < N; ++i) {
    const size_t p = (i * 17) % UNIQUE_POINTS;
    lats[i] = base_lats[p];
    lngs[i] = base_lngs[p];
  }

  std::vector<uint64_t> cells(N, 0);
  std::vector<uint8_t> valid(N, 0);
  pgaccel_status s = pgaccel_h3_lat_lng_to_cell_bulk(lats.data(), lngs.data(), N, RESOLUTION,
                                                     /*use_fp64=*/1, cells.data(), valid.data());
  ASSERT_STATUS_OK("lat_lng_count reference cell status", s);

  std::unordered_map<uint64_t, int64_t> expected;
  bool all_valid = (s == PGACCEL_OK);
  for (size_t i = 0; i < N && all_valid; ++i) {
    if (valid[i] == 0 || cells[i] == 0) {
      all_valid = false;
      break;
    }
    expected[cells[i]] += 1;
  }
  ASSERT_TRUE("lat_lng_count reference cells all valid", all_valid);

  pgaccel_agg_state* state = nullptr;
  pgaccel_reset_gpu_exec_count();
  s = pgaccel_h3_lat_lng_count_bulk(lats.data(), lngs.data(), N, RESOLUTION, &state);
  ASSERT_STATUS_OK("lat_lng_count fused status", s);
  ASSERT_TRUE("lat_lng_count fused state non-null", state != nullptr);
  ASSERT_TRUE("lat_lng_count fused launched GPU kernels", pgaccel_gpu_exec_count() > 0);
  if (state == nullptr) {
    return;
  }

  ASSERT_EQ("lat_lng_count fused group count", pgaccel_agg_group_count(state), expected.size());
  const auto* keys_out = static_cast<const int64_t*>(pgaccel_agg_get_group_keys(state));
  const double* counts = pgaccel_agg_get_results(state, 0);
  const int64_t* row_counts = pgaccel_agg_get_counts(state);
  ASSERT_TRUE("lat_lng_count fused output buffers non-null",
              keys_out != nullptr && counts != nullptr && row_counts != nullptr);

  bool counts_match = true;
  int64_t total_counts = 0;
  int64_t total_row_counts = 0;
  bool saw_duplicate_group = false;
  if (keys_out != nullptr && counts != nullptr && row_counts != nullptr) {
    for (size_t g = 0; g < pgaccel_agg_group_count(state); ++g) {
      const uint64_t cell = static_cast<uint64_t>(keys_out[g]);
      auto it = expected.find(cell);
      if (it == expected.end() || std::abs(counts[g] - static_cast<double>(it->second)) > 1e-9 ||
          row_counts[g] != it->second) {
        counts_match = false;
        break;
      }
      total_counts += static_cast<int64_t>(counts[g]);
      total_row_counts += row_counts[g];
      saw_duplicate_group = saw_duplicate_group || row_counts[g] > 1;
    }
  }
  ASSERT_TRUE("lat_lng_count fused counts match reference cells", counts_match);
  ASSERT_EQ("lat_lng_count fused count total", total_counts, static_cast<int64_t>(N));
  ASSERT_EQ("lat_lng_count fused row-count total", total_row_counts, static_cast<int64_t>(N));
  ASSERT_TRUE("lat_lng_count duplicate groups preserved", saw_duplicate_group);

  pgaccel_agg_free(state);

  const double invalid_lat = 91.0;
  const double valid_lng = 0.0;
  for (int resolution : {3, 10}) {
    state = reinterpret_cast<pgaccel_agg_state*>(uintptr_t{1});
    s = pgaccel_h3_lat_lng_count_bulk(&invalid_lat, &valid_lng, 1, resolution, &state);
    char label[128];
    snprintf(label, sizeof(label), "lat_lng_count invalid coordinate res=%d", resolution);
    ASSERT_EQ(label, s, PGACCEL_ERROR);
    snprintf(label, sizeof(label), "lat_lng_count invalid res=%d clears state", resolution);
    ASSERT_TRUE(label, state == nullptr);
  }
}

static void
assert_count_state_matches_expected(pgaccel_agg_state* state,
                                    const std::unordered_map<uint64_t, int64_t>& expected,
                                    size_t expected_total, int resolution, const char* label) {
  char buf[160];
  snprintf(buf, sizeof(buf), "%s res=%d group count", label, resolution);
  ASSERT_EQ(buf, pgaccel_agg_group_count(state), expected.size());

  const auto* keys_out = static_cast<const int64_t*>(pgaccel_agg_get_group_keys(state));
  const double* counts = pgaccel_agg_get_results(state, 0);
  const int64_t* row_counts = pgaccel_agg_get_counts(state);
  snprintf(buf, sizeof(buf), "%s res=%d output buffers", label, resolution);
  ASSERT_TRUE(buf, keys_out != nullptr && counts != nullptr && row_counts != nullptr);

  bool counts_match = true;
  int64_t total_counts = 0;
  int64_t total_row_counts = 0;
  bool saw_duplicate_group = false;
  if (keys_out != nullptr && counts != nullptr && row_counts != nullptr) {
    for (size_t g = 0; g < pgaccel_agg_group_count(state); ++g) {
      const uint64_t cell = static_cast<uint64_t>(keys_out[g]);
      auto it = expected.find(cell);
      if (it == expected.end() || std::abs(counts[g] - static_cast<double>(it->second)) > 1e-9 ||
          row_counts[g] != it->second) {
        counts_match = false;
        break;
      }
      total_counts += static_cast<int64_t>(counts[g]);
      total_row_counts += row_counts[g];
      saw_duplicate_group = saw_duplicate_group || row_counts[g] > 1;
    }
  }

  snprintf(buf, sizeof(buf), "%s res=%d counts match reference", label, resolution);
  ASSERT_TRUE(buf, counts_match);
  snprintf(buf, sizeof(buf), "%s res=%d count total", label, resolution);
  ASSERT_EQ(buf, total_counts, static_cast<int64_t>(expected_total));
  snprintf(buf, sizeof(buf), "%s res=%d row-count total", label, resolution);
  ASSERT_EQ(buf, total_row_counts, static_cast<int64_t>(expected_total));
  snprintf(buf, sizeof(buf), "%s res=%d duplicate groups", label, resolution);
  ASSERT_TRUE(buf, saw_duplicate_group);
}

static void test_cell_to_parent_count_bulk() {
  printf("--- test_cell_to_parent_count_bulk ---\n");

  const int parent_res = 2;
  std::vector<uint64_t> cells;
  cells.reserve(6);

  int d0[15] = {1, 2, 0, 1, 2, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0};
  int d1[15] = {1, 2, 3, 4, 5, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0};
  int d2[15] = {1, 2, 5, 4, 3, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0};
  int d3[15] = {4, 5, 1, 1, 1, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0};
  int d4[15] = {4, 5, 2, 2, 2, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0};
  int d5[15] = {0, 1, 2, 3, 4, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0};
  cells.push_back(make_cell(10, 5, d0));
  cells.push_back(make_cell(10, 5, d1));
  cells.push_back(make_cell(10, 5, d2));
  cells.push_back(make_cell(10, 5, d3));
  cells.push_back(make_cell(10, 5, d4));
  cells.push_back(make_cell(10, 5, d5));

  std::vector<uint64_t> parents(cells.size(), 0);
  pgaccel_status s =
      pgaccel_h3_cell_to_parent_bulk(cells.data(), cells.size(), parent_res, parents.data());
  ASSERT_STATUS_OK("cell_to_parent_count reference parent status", s);

  std::unordered_map<uint64_t, int64_t> expected;
  bool all_valid = (s == PGACCEL_OK);
  for (uint64_t parent : parents) {
    if (parent == 0) {
      all_valid = false;
      break;
    }
    expected[parent] += 1;
  }
  ASSERT_TRUE("cell_to_parent_count reference parents all valid", all_valid);

  pgaccel_agg_state* state = nullptr;
  pgaccel_reset_gpu_exec_count();
  s = pgaccel_h3_cell_to_parent_count_bulk(cells.data(), cells.size(), parent_res, &state);
  ASSERT_STATUS_OK("cell_to_parent_count fused status", s);
  ASSERT_TRUE("cell_to_parent_count fused state non-null", state != nullptr);
  ASSERT_TRUE("cell_to_parent_count fused launched GPU kernels", pgaccel_gpu_exec_count() > 0);
  if (state == nullptr) {
    return;
  }

  assert_count_state_matches_expected(state, expected, cells.size(), parent_res,
                                      "cell_to_parent_count");
  pgaccel_agg_free(state);

  uint64_t invalid_cells[] = {0, cells[0]};
  state = reinterpret_cast<pgaccel_agg_state*>(uintptr_t{1});
  s = pgaccel_h3_cell_to_parent_count_bulk(invalid_cells, 2, parent_res, &state);
  ASSERT_EQ("cell_to_parent_count rejects zero cell", s, PGACCEL_ERROR);
  ASSERT_TRUE("cell_to_parent_count zero cell clears state", state == nullptr);

  const uint64_t non_cell_mode = cells[0] & ~(UINT64_C(0xf) << 59);
  state = reinterpret_cast<pgaccel_agg_state*>(uintptr_t{1});
  s = pgaccel_h3_cell_to_parent_count_bulk(&non_cell_mode, 1, parent_res, &state);
  ASSERT_EQ("cell_to_parent_count rejects nonzero malformed cell", s, PGACCEL_ERROR);
  ASSERT_TRUE("cell_to_parent_count malformed cell clears state", state == nullptr);

  int coarse_digits[15] = {2, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0};
  const uint64_t coarse_cell = make_cell(10, 1, coarse_digits);
  state = reinterpret_cast<pgaccel_agg_state*>(uintptr_t{1});
  s = pgaccel_h3_cell_to_parent_count_bulk(&coarse_cell, 1, parent_res, &state);
  ASSERT_EQ("cell_to_parent_count rejects resolution mismatch", s, PGACCEL_ERROR);
  ASSERT_TRUE("cell_to_parent_count resolution mismatch clears state", state == nullptr);
}

static void assert_lat_lng_count_matches_reference(const std::vector<double>& lats,
                                                   const std::vector<double>& lngs, int resolution,
                                                   const char* label) {
  ASSERT_TRUE("lat_lng_count helper aligned input", lats.size() == lngs.size());
  const size_t N = lats.size();

  std::vector<uint64_t> cells(N, 0);
  std::vector<uint8_t> valid(N, 0);
  pgaccel_status s = pgaccel_h3_lat_lng_to_cell_bulk(lats.data(), lngs.data(), N, resolution,
                                                     /*use_fp64=*/1, cells.data(), valid.data());
  char buf[160];
  snprintf(buf, sizeof(buf), "%s reference cells res=%d status", label, resolution);
  ASSERT_STATUS_OK(buf, s);

  std::unordered_map<uint64_t, int64_t> expected;
  bool all_valid = (s == PGACCEL_OK);
  for (size_t i = 0; i < N && all_valid; ++i) {
    if (valid[i] == 0 || cells[i] == 0) {
      all_valid = false;
      break;
    }
    expected[cells[i]] += 1;
  }
  snprintf(buf, sizeof(buf), "%s reference cells res=%d all valid", label, resolution);
  ASSERT_TRUE(buf, all_valid);

  pgaccel_agg_state* state = nullptr;
  s = pgaccel_h3_lat_lng_count_bulk(lats.data(), lngs.data(), N, resolution, &state);
  snprintf(buf, sizeof(buf), "%s fused count res=%d status", label, resolution);
  ASSERT_STATUS_OK(buf, s);
  snprintf(buf, sizeof(buf), "%s fused count res=%d state non-null", label, resolution);
  ASSERT_TRUE(buf, state != nullptr);
  if (state == nullptr) {
    return;
  }

  assert_count_state_matches_expected(state, expected, N, resolution, label);

  pgaccel_agg_free(state);
}

static void assert_lat_lng_count_f32_exact_matches_reference(const std::vector<double>& lats,
                                                             const std::vector<double>& lngs,
                                                             int resolution, const char* label) {
  ASSERT_TRUE("lat_lng_count f32/exact helper aligned input", lats.size() == lngs.size());
  const size_t N = lats.size();

  std::vector<uint64_t> cells(N, 0);
  std::vector<uint8_t> valid(N, 0);
  pgaccel_status s = pgaccel_h3_lat_lng_to_cell_bulk(lats.data(), lngs.data(), N, resolution,
                                                     /*use_fp64=*/1, cells.data(), valid.data());
  char buf[192];
  snprintf(buf, sizeof(buf), "%s f32/exact reference res=%d status", label, resolution);
  ASSERT_STATUS_OK(buf, s);

  std::unordered_map<uint64_t, int64_t> expected;
  bool all_valid = (s == PGACCEL_OK);
  for (size_t i = 0; i < N && all_valid; ++i) {
    if (valid[i] == 0 || cells[i] == 0) {
      all_valid = false;
      break;
    }
    expected[cells[i]] += 1;
  }
  snprintf(buf, sizeof(buf), "%s f32/exact reference res=%d all valid", label, resolution);
  ASSERT_TRUE(buf, all_valid);

  std::vector<float> lats_f32(N);
  std::vector<float> lngs_f32(N);
  for (size_t i = 0; i < N; ++i) {
    lats_f32[i] = static_cast<float>(lats[i]);
    lngs_f32[i] = static_cast<float>(lngs[i]);
  }

  pgaccel_agg_state* state = nullptr;
  s = pgaccel_h3_lat_lng_count_bulk_f32_exact(lats_f32.data(), lngs_f32.data(), lats.data(),
                                              lngs.data(), N, resolution, &state);
  snprintf(buf, sizeof(buf), "%s f32/exact count res=%d status", label, resolution);
  ASSERT_STATUS_OK(buf, s);
  snprintf(buf, sizeof(buf), "%s f32/exact count res=%d state non-null", label, resolution);
  ASSERT_TRUE(buf, state != nullptr);
  if (state == nullptr) {
    return;
  }

  assert_count_state_matches_expected(state, expected, N, resolution, label);

  pgaccel_agg_free(state);
}

static void test_lat_lng_count_bulk_all_res_duplicate_edges() {
  printf("--- test_lat_lng_count_bulk_all_res_duplicate_edges ---\n");

  const double unique_lats[] = {
      0.0,
      0.0,
      0.0,
      37.7749,
      -33.8688,
      51.5074,
      89.9999,
      -89.9999,
      45.0,
      45.0,
      -45.0,
      -45.0,
      12.3456,
      -12.3456,
      66.1234,
      -66.1234,
      23.4567,
      -23.4567,
      10.0,
      -10.0,
      35.659494,
      -33.856159,
      48.858844,
      40.689247,
      13.016968360646686,
      -34.509355353769905,
      45.8732886135455,
      -75.49413973183843,
      74.81190977496243,
      46.185213710164106,
  };
  const double unique_lngs[] = {
      0.0,
      90.0,
      -90.0,
      -122.4194,
      151.2093,
      -0.1278,
      0.0,
      0.0,
      179.9999,
      -179.9999,
      179.9999,
      -179.9999,
      179.999,
      -179.999,
      45.6789,
      -45.6789,
      123.4567,
      -123.4567,
      179.5,
      -179.5,
      139.700472,
      151.215256,
      2.294351,
      -74.044502,
      -150.76193454469657,
      -10.863254691148114,
      71.43813151687368,
      -32.98890540892165,
      145.64017648341706,
      71.57706904291612,
  };
  constexpr size_t UNIQUE = sizeof(unique_lats) / sizeof(unique_lats[0]);
  constexpr size_t N = 384;

  std::vector<double> lats(N);
  std::vector<double> lngs(N);
  for (size_t i = 0; i < N; ++i) {
    const size_t p = (i * 11 + 7) % UNIQUE;
    lats[i] = unique_lats[p];
    lngs[i] = unique_lngs[p];
  }

  for (int res = 0; res <= 15; ++res) {
    assert_lat_lng_count_matches_reference(lats, lngs, res, "all-res duplicate edge count");
  }
}

static void test_lat_lng_count_bulk_f32_exact_all_res_edge_randomized() {
  printf("--- test_lat_lng_count_bulk_f32_exact_all_res_edge_randomized ---\n");

  std::vector<double> seed_lats;
  std::vector<double> seed_lngs;
  h3_add_edge_coverage_points(seed_lats, seed_lngs);
  h3_add_deterministic_random_points(seed_lats, seed_lngs, 48);

  std::vector<double> lats;
  std::vector<double> lngs;
  lats.reserve(seed_lats.size() * 2);
  lngs.reserve(seed_lngs.size() * 2);
  for (size_t i = 0; i < seed_lats.size(); ++i) {
    lats.push_back(seed_lats[i]);
    lngs.push_back(seed_lngs[i]);
    if (i % 3 == 0 || i % 11 == 0) {
      lats.push_back(seed_lats[i]);
      lngs.push_back(seed_lngs[i]);
    }
  }

  ASSERT_TRUE("f32/exact all-res edge input duplicates present", lats.size() > seed_lats.size());
  for (int res = 0; res <= 15; ++res) {
    assert_lat_lng_count_f32_exact_matches_reference(lats, lngs, res,
                                                     "all-res edge/random f32 exact count");
  }

  const float invalid_lat_f32 = 91.0f;
  const float valid_lng_f32 = 0.0f;
  const double invalid_lat = 91.0;
  const double valid_lng = 0.0;
  pgaccel_agg_state* state = reinterpret_cast<pgaccel_agg_state*>(uintptr_t{1});
  const pgaccel_status status = pgaccel_h3_lat_lng_count_bulk_f32_exact(
      &invalid_lat_f32, &valid_lng_f32, &invalid_lat, &valid_lng, 1, 3, &state);
  ASSERT_EQ("f32 exact count rejects invalid coordinate", status, PGACCEL_ERROR);
  ASSERT_TRUE("f32 exact invalid coordinate clears state", state == nullptr);
}

static void test_lat_lng_count_resident_low_high_matrix() {
  printf("--- test_lat_lng_count_resident_low_high_matrix ---\n");

  std::vector<double> lats;
  std::vector<double> lngs;
  h3_add_edge_coverage_points(lats, lngs);
  h3_add_deterministic_random_points(lats, lngs, 256);
  const size_t unique_count = lats.size();
  for (size_t i = 0; i < unique_count; i += 5) {
    lats.push_back(lats[i]);
    lngs.push_back(lngs[i]);
  }

  std::vector<float> lats_f32(lats.size());
  std::vector<float> lngs_f32(lngs.size());
  for (size_t i = 0; i < lats.size(); ++i) {
    lats_f32[i] = static_cast<float>(lats[i]);
    lngs_f32[i] = static_cast<float>(lngs[i]);
  }

  DeviceResidentAllocation device_lats(lats.data(), lats.size() * sizeof(double));
  DeviceResidentAllocation device_lngs(lngs.data(), lngs.size() * sizeof(double));
  DeviceResidentAllocation device_lats_f32(lats_f32.data(), lats_f32.size() * sizeof(float));
  DeviceResidentAllocation device_lngs_f32(lngs_f32.data(), lngs_f32.size() * sizeof(float));

  for (int resolution : {3, 10}) {
    std::vector<uint64_t> reference_cells(lats.size(), 0);
    std::vector<uint8_t> reference_valid(lats.size(), 0);
    pgaccel_status status = pgaccel_h3_lat_lng_to_cell_bulk(
        lats.data(), lngs.data(), lats.size(), resolution, /*use_fp64=*/1, reference_cells.data(),
        reference_valid.data());
    char label[192];
    snprintf(label, sizeof(label), "resident count res=%d reference status", resolution);
    ASSERT_STATUS_OK(label, status);

    std::unordered_map<uint64_t, int64_t> expected;
    bool reference_ok = true;
    for (size_t i = 0; i < reference_cells.size(); ++i) {
      reference_ok = reference_ok && reference_valid[i] == 1 && reference_cells[i] != 0;
      expected[reference_cells[i]] += 1;
    }
    snprintf(label, sizeof(label), "resident count res=%d reference valid", resolution);
    ASSERT_TRUE(label, reference_ok);

    pgaccel_agg_state* state = nullptr;
    pgaccel_reset_gpu_exec_count();
    status = pgaccel_h3_lat_lng_count_resident_bulk(
        static_cast<const double*>(device_lats.data()),
        static_cast<const double*>(device_lngs.data()),
        static_cast<const float*>(device_lats_f32.data()),
        static_cast<const float*>(device_lngs_f32.data()), lats.size(), resolution, &state);
    snprintf(label, sizeof(label), "resident count res=%d status", resolution);
    ASSERT_STATUS_OK(label, status);
    snprintf(label, sizeof(label), "resident count res=%d dispatched", resolution);
    ASSERT_TRUE(label, pgaccel_gpu_exec_count() > 0);
    snprintf(label, sizeof(label), "resident count res=%d state", resolution);
    ASSERT_TRUE(label, state != nullptr);
    if (state != nullptr) {
      assert_count_state_matches_expected(state, expected, lats.size(), resolution,
                                          "resident lat/lng count");
      pgaccel_agg_free(state);
    }
  }

  std::vector<double> copied_lats(lats.size(), 0.0);
  std::vector<float> copied_lngs_f32(lngs_f32.size(), 0.0f);
  ASSERT_STATUS_OK("resident exact input readback",
                   pgaccel_expr_device_copy_to_host(copied_lats.data(), device_lats.data(),
                                                    device_lats.bytes()));
  ASSERT_STATUS_OK("resident fp32 input readback",
                   pgaccel_expr_device_copy_to_host(copied_lngs_f32.data(), device_lngs_f32.data(),
                                                    device_lngs_f32.bytes()));
  ASSERT_TRUE("resident exact input preserved", copied_lats == lats);
  ASSERT_TRUE("resident fp32 input preserved", copied_lngs_f32 == lngs_f32);

  const double invalid_lat = 91.0;
  const double valid_lng = 0.0;
  const float invalid_lat_f32 = 91.0f;
  const float valid_lng_f32 = 0.0f;
  DeviceResidentAllocation device_invalid_lat(&invalid_lat, sizeof(invalid_lat));
  DeviceResidentAllocation device_valid_lng(&valid_lng, sizeof(valid_lng));
  DeviceResidentAllocation device_invalid_lat_f32(&invalid_lat_f32, sizeof(invalid_lat_f32));
  DeviceResidentAllocation device_valid_lng_f32(&valid_lng_f32, sizeof(valid_lng_f32));
  for (int resolution : {3, 10}) {
    pgaccel_agg_state* state = reinterpret_cast<pgaccel_agg_state*>(uintptr_t{1});
    const pgaccel_status status = pgaccel_h3_lat_lng_count_resident_bulk(
        static_cast<const double*>(device_invalid_lat.data()),
        static_cast<const double*>(device_valid_lng.data()),
        static_cast<const float*>(device_invalid_lat_f32.data()),
        static_cast<const float*>(device_valid_lng_f32.data()), 1, resolution, &state);
    char label[192];
    snprintf(label, sizeof(label), "resident invalid coordinate res=%d rejected", resolution);
    ASSERT_EQ(label, status, PGACCEL_ERROR);
    snprintf(label, sizeof(label), "resident invalid coordinate res=%d clears state", resolution);
    ASSERT_TRUE(label, state == nullptr);
  }

  // Low-resolution resident input is screened by the fp32 kernel before exact
  // fixups. Exercise every coordinate rejection arm, including non-finite
  // input, through the device-pointer API.
  const double nan_f64 = std::numeric_limits<double>::quiet_NaN();
  const float nan_f32 = std::numeric_limits<float>::quiet_NaN();
  const double invalid_exact_lats[] = {-91.0, 0.0, 0.0, nan_f64};
  const double invalid_exact_lngs[] = {0.0, -181.0, 181.0, 0.0};
  const float invalid_fast_lats[] = {-91.0f, 0.0f, 0.0f, nan_f32};
  const float invalid_fast_lngs[] = {0.0f, -181.0f, 181.0f, 0.0f};
  constexpr size_t invalid_count = std::size(invalid_exact_lats);
  DeviceResidentAllocation device_invalid_exact_lats(invalid_exact_lats,
                                                     sizeof(invalid_exact_lats));
  DeviceResidentAllocation device_invalid_exact_lngs(invalid_exact_lngs,
                                                     sizeof(invalid_exact_lngs));
  DeviceResidentAllocation device_invalid_fast_lats(invalid_fast_lats, sizeof(invalid_fast_lats));
  DeviceResidentAllocation device_invalid_fast_lngs(invalid_fast_lngs, sizeof(invalid_fast_lngs));
  pgaccel_agg_state* invalid_state = reinterpret_cast<pgaccel_agg_state*>(uintptr_t{1});
  pgaccel_reset_gpu_exec_count();
  pgaccel_status invalid_status = pgaccel_h3_lat_lng_count_resident_bulk(
      static_cast<const double*>(device_invalid_exact_lats.data()),
      static_cast<const double*>(device_invalid_exact_lngs.data()),
      static_cast<const float*>(device_invalid_fast_lats.data()),
      static_cast<const float*>(device_invalid_fast_lngs.data()), invalid_count, 3, &invalid_state);
  ASSERT_EQ("resident fp32 invalid matrix rejected", invalid_status, PGACCEL_ERROR);
  ASSERT_TRUE("resident fp32 invalid matrix clears state", invalid_state == nullptr);
  ASSERT_TRUE("resident fp32 invalid matrix dispatched", pgaccel_gpu_exec_count() > 0);

  // The edge corpus contains fp32 rows marked for exact correction. Supplying
  // an invalid exact sidecar verifies that those rows cannot publish the fast
  // candidate when exact projection rejects them.
  std::vector<double> rejected_exact_lats(lats.size(), 91.0);
  DeviceResidentAllocation device_rejected_exact_lats(rejected_exact_lats.data(),
                                                      rejected_exact_lats.size() * sizeof(double));
  pgaccel_agg_state* rejected_state = reinterpret_cast<pgaccel_agg_state*>(uintptr_t{1});
  pgaccel_reset_gpu_exec_count();
  const pgaccel_status rejected_status = pgaccel_h3_lat_lng_count_resident_bulk(
      static_cast<const double*>(device_rejected_exact_lats.data()),
      static_cast<const double*>(device_lngs.data()),
      static_cast<const float*>(device_lats_f32.data()),
      static_cast<const float*>(device_lngs_f32.data()), lats.size(), 3, &rejected_state);
  ASSERT_EQ("resident exact sidecar rejection status", rejected_status, PGACCEL_ERROR);
  ASSERT_TRUE("resident exact sidecar rejection clears state", rejected_state == nullptr);
  ASSERT_TRUE("resident exact sidecar rejection dispatched", pgaccel_gpu_exec_count() > 0);
}

static void test_lat_lng_res7_exact_edge_fixups() {
  printf("--- test_lat_lng_res7_exact_edge_fixups ---\n");

  // Rows captured from the deterministic 1M `h3_bulk` benchmark fixture
  // (`setseed(0.000042)`). The expected cells are h3-pg
  // `public.h3_lat_lng_to_cell(point(lng, lat), 7)` outputs. These points sit
  // on resolution-7 boundaries where the fp32 path can choose an adjacent cell
  // unless the exact correction pass runs.
  const double lats[] = {
      13.016968360646686, -34.509355353769905, 45.8732886135455,
      -75.49413973183843, 74.81190977496243,   46.185213710164106,
  };
  const double lngs[] = {
      -150.76193454469657, -10.863254691148114, 71.43813151687368,
      -32.98890540892165,  145.64017648341706,  71.57706904291612,
  };
  const uint64_t expected_cells[] = {
      UINT64_C(0x875c00385ffffff), UINT64_C(0x87c000130ffffff), UINT64_C(0x8720000b1ffffff),
      UINT64_C(0x87ee06aebffffff), UINT64_C(0x8704000f0ffffff), UINT64_C(0x872000143ffffff),
  };
  constexpr size_t N = sizeof(lats) / sizeof(lats[0]);

  std::vector<uint64_t> cells(N, 0);
  std::vector<uint8_t> valid(N, 0);
  pgaccel_status s =
      pgaccel_h3_lat_lng_to_cell_bulk(lats, lngs, N, 7, /*use_fp64=*/1, cells.data(), valid.data());
  ASSERT_STATUS_OK("res7 exact edge cell status", s);
  bool cells_match = true;
  for (size_t i = 0; i < N; ++i) {
    cells_match = cells_match && valid[i] != 0 && cells[i] == expected_cells[i];
  }
  ASSERT_TRUE("res7 exact edge cells match h3-pg", cells_match);

  pgaccel_agg_state* state = nullptr;
  s = pgaccel_h3_lat_lng_count_bulk(lats, lngs, N, 7, &state);
  ASSERT_STATUS_OK("res7 exact edge count status", s);
  ASSERT_TRUE("res7 exact edge count state", state != nullptr);
  if (state == nullptr) {
    return;
  }

  ASSERT_EQ("res7 exact edge count group count", pgaccel_agg_group_count(state), N);
  const auto* keys_out = static_cast<const int64_t*>(pgaccel_agg_get_group_keys(state));
  const int64_t* row_counts = pgaccel_agg_get_counts(state);
  ASSERT_TRUE("res7 exact edge count buffers", keys_out != nullptr && row_counts != nullptr);
  bool groups_match = true;
  if (keys_out != nullptr && row_counts != nullptr) {
    std::unordered_map<uint64_t, int64_t> groups;
    for (size_t g = 0; g < pgaccel_agg_group_count(state); ++g) {
      groups[static_cast<uint64_t>(keys_out[g])] += row_counts[g];
    }
    for (size_t i = 0; i < N; ++i) {
      groups_match = groups_match && groups[expected_cells[i]] == 1;
    }
  }
  ASSERT_TRUE("res7 exact edge count groups match h3-pg cells", groups_match);
  pgaccel_agg_free(state);

  std::vector<float> lats_f32(N);
  std::vector<float> lngs_f32(N);
  for (size_t i = 0; i < N; ++i) {
    lats_f32[i] = static_cast<float>(lats[i]);
    lngs_f32[i] = static_cast<float>(lngs[i]);
  }

  pgaccel_agg_state* f32_state = nullptr;
  s = pgaccel_h3_lat_lng_count_bulk_f32_exact(lats_f32.data(), lngs_f32.data(), lats, lngs, N, 7,
                                              &f32_state);
  ASSERT_STATUS_OK("res7 f32/exact edge count status", s);
  ASSERT_TRUE("res7 f32/exact edge count state", f32_state != nullptr);
  if (f32_state == nullptr) {
    return;
  }

  ASSERT_EQ("res7 f32/exact edge count group count", pgaccel_agg_group_count(f32_state), N);
  const auto* f32_keys_out = static_cast<const int64_t*>(pgaccel_agg_get_group_keys(f32_state));
  const int64_t* f32_row_counts = pgaccel_agg_get_counts(f32_state);
  ASSERT_TRUE("res7 f32/exact edge count buffers",
              f32_keys_out != nullptr && f32_row_counts != nullptr);
  bool f32_groups_match = true;
  if (f32_keys_out != nullptr && f32_row_counts != nullptr) {
    std::unordered_map<uint64_t, int64_t> groups;
    for (size_t g = 0; g < pgaccel_agg_group_count(f32_state); ++g) {
      groups[static_cast<uint64_t>(f32_keys_out[g])] += f32_row_counts[g];
    }
    for (size_t i = 0; i < N; ++i) {
      f32_groups_match = f32_groups_match && groups[expected_cells[i]] == 1;
    }
  }
  ASSERT_TRUE("res7 f32/exact edge count groups match h3-pg cells", f32_groups_match);
  pgaccel_agg_free(f32_state);
}

// ---------------------------------------------------------------------------
// Test: null pointer handling
// ---------------------------------------------------------------------------
static void test_null_pointers() {
  printf("--- test_null_pointers ---\n");

  pgaccel_status s;

  s = pgaccel_h3_get_resolution_bulk(nullptr, 5, nullptr);
  ASSERT_EQ("get_resolution null", s, PGACCEL_ERROR_INIT);

  s = pgaccel_h3_cell_to_parent_bulk(nullptr, 5, 0, nullptr);
  ASSERT_EQ("cell_to_parent null", s, PGACCEL_ERROR_INIT);

  s = pgaccel_h3_cell_to_parent_count_bulk(nullptr, 5, 0, nullptr);
  ASSERT_EQ("cell_to_parent_count null", s, PGACCEL_ERROR_INIT);

  s = pgaccel_h3_grid_distance_bulk(nullptr, nullptr, 5, nullptr);
  ASSERT_EQ("grid_distance null", s, PGACCEL_ERROR_INIT);

  double lat = 0.0, lng = 0.0;
  s = pgaccel_h3_lat_lng_to_cell_bulk(&lat, &lng, 5, 0, true, nullptr, nullptr);
  ASSERT_EQ("lat_lng_to_cell null output", s, PGACCEL_ERROR_INIT);

  s = pgaccel_h3_lat_lng_to_cell_bulk(nullptr, nullptr, 5, 0, true, nullptr, nullptr);
  ASSERT_EQ("lat_lng_to_cell null input", s, PGACCEL_ERROR_INIT);
}

static void test_api_contract_matrix() {
  printf("--- test_api_contract_matrix ---\n");

  int digits[15] = {0};
  const uint64_t cell = make_cell(57, 3, digits);
  int32_t i32_out = -1;
  uint8_t u8_out = 99;
  uint64_t u64_out = UINT64_MAX;
  double lat = 0.0;
  double lng = 0.0;
  float lat_f32 = 0.0f;
  float lng_f32 = 0.0f;
  pgaccel_agg_state* state = reinterpret_cast<pgaccel_agg_state*>(uintptr_t{1});

  ASSERT_STATUS_OK("get_base empty", pgaccel_h3_get_base_cell_bulk(nullptr, 0, nullptr));
  ASSERT_EQ("get_base null input", pgaccel_h3_get_base_cell_bulk(nullptr, 1, &i32_out),
            PGACCEL_ERROR_INIT);
  ASSERT_EQ("get_base null output", pgaccel_h3_get_base_cell_bulk(&cell, 1, nullptr),
            PGACCEL_ERROR_INIT);
  ASSERT_STATUS_OK("is_valid empty", pgaccel_h3_is_valid_cell_bulk(nullptr, 0, nullptr));
  ASSERT_EQ("is_valid null input", pgaccel_h3_is_valid_cell_bulk(nullptr, 1, &u8_out),
            PGACCEL_ERROR_INIT);
  ASSERT_EQ("is_valid null output", pgaccel_h3_is_valid_cell_bulk(&cell, 1, nullptr),
            PGACCEL_ERROR_INIT);
  ASSERT_STATUS_OK("is_pentagon empty", pgaccel_h3_is_pentagon_bulk(nullptr, 0, nullptr));
  ASSERT_EQ("is_pentagon null input", pgaccel_h3_is_pentagon_bulk(nullptr, 1, &u8_out),
            PGACCEL_ERROR_INIT);
  ASSERT_EQ("is_pentagon null output", pgaccel_h3_is_pentagon_bulk(&cell, 1, nullptr),
            PGACCEL_ERROR_INIT);
  ASSERT_STATUS_OK("class III empty", pgaccel_h3_is_res_class_iii_bulk(nullptr, 0, nullptr));
  ASSERT_EQ("class III null input", pgaccel_h3_is_res_class_iii_bulk(nullptr, 1, &u8_out),
            PGACCEL_ERROR_INIT);
  ASSERT_EQ("class III null output", pgaccel_h3_is_res_class_iii_bulk(&cell, 1, nullptr),
            PGACCEL_ERROR_INIT);
  ASSERT_STATUS_OK("center child empty",
                   pgaccel_h3_cell_to_center_child_bulk(nullptr, 0, 3, nullptr));
  ASSERT_EQ("center child null input",
            pgaccel_h3_cell_to_center_child_bulk(nullptr, 1, 3, &u64_out), PGACCEL_ERROR_INIT);
  ASSERT_EQ("center child null output", pgaccel_h3_cell_to_center_child_bulk(&cell, 1, 3, nullptr),
            PGACCEL_ERROR_INIT);
  ASSERT_EQ("center child negative resolution",
            pgaccel_h3_cell_to_center_child_bulk(&cell, 1, -1, &u64_out),
            PGACCEL_ERROR_UNSUPPORTED);

  state = reinterpret_cast<pgaccel_agg_state*>(uintptr_t{1});
  ASSERT_STATUS_OK("parent count empty",
                   pgaccel_h3_cell_to_parent_count_bulk(nullptr, 0, 0, &state));
  ASSERT_TRUE("parent count empty clears state", state == nullptr);
  ASSERT_EQ("parent count null state", pgaccel_h3_cell_to_parent_count_bulk(&cell, 1, 0, nullptr),
            PGACCEL_ERROR_INIT);
  ASSERT_EQ("parent count null cells", pgaccel_h3_cell_to_parent_count_bulk(nullptr, 1, 0, &state),
            PGACCEL_ERROR_INIT);
  ASSERT_EQ("parent count invalid resolution",
            pgaccel_h3_cell_to_parent_count_bulk(&cell, 1, 16, &state), PGACCEL_ERROR_UNSUPPORTED);

  state = reinterpret_cast<pgaccel_agg_state*>(uintptr_t{1});
  ASSERT_STATUS_OK("lat/lng count empty",
                   pgaccel_h3_lat_lng_count_bulk(nullptr, nullptr, 0, 3, &state));
  ASSERT_TRUE("lat/lng count empty clears state", state == nullptr);
  ASSERT_EQ("lat/lng count null state", pgaccel_h3_lat_lng_count_bulk(&lat, &lng, 1, 3, nullptr),
            PGACCEL_ERROR_INIT);
  ASSERT_EQ("lat/lng count null coordinates",
            pgaccel_h3_lat_lng_count_bulk(nullptr, &lng, 1, 3, &state), PGACCEL_ERROR_INIT);
  ASSERT_EQ("lat/lng count invalid resolution",
            pgaccel_h3_lat_lng_count_bulk(&lat, &lng, 1, -1, &state), PGACCEL_ERROR_UNSUPPORTED);

  state = reinterpret_cast<pgaccel_agg_state*>(uintptr_t{1});
  ASSERT_STATUS_OK("f32 exact count empty", pgaccel_h3_lat_lng_count_bulk_f32_exact(
                                                nullptr, nullptr, nullptr, nullptr, 0, 3, &state));
  ASSERT_TRUE("f32 exact count empty clears state", state == nullptr);
  ASSERT_EQ("f32 exact count null state",
            pgaccel_h3_lat_lng_count_bulk_f32_exact(&lat_f32, &lng_f32, &lat, &lng, 1, 3, nullptr),
            PGACCEL_ERROR_INIT);
  ASSERT_EQ("f32 exact count null coordinates",
            pgaccel_h3_lat_lng_count_bulk_f32_exact(nullptr, &lng_f32, &lat, &lng, 1, 3, &state),
            PGACCEL_ERROR_INIT);
  ASSERT_EQ("f32 exact count invalid resolution",
            pgaccel_h3_lat_lng_count_bulk_f32_exact(&lat_f32, &lng_f32, &lat, &lng, 1, 16, &state),
            PGACCEL_ERROR_UNSUPPORTED);

  state = reinterpret_cast<pgaccel_agg_state*>(uintptr_t{1});
  ASSERT_STATUS_OK("resident count empty", pgaccel_h3_lat_lng_count_resident_bulk(
                                               nullptr, nullptr, nullptr, nullptr, 0, 3, &state));
  ASSERT_TRUE("resident count empty clears state", state == nullptr);
  ASSERT_EQ("resident count null state",
            pgaccel_h3_lat_lng_count_resident_bulk(&lat, &lng, &lat_f32, &lng_f32, 1, 3, nullptr),
            PGACCEL_ERROR_INIT);
  ASSERT_EQ("resident count null coordinate",
            pgaccel_h3_lat_lng_count_resident_bulk(nullptr, &lng, &lat_f32, &lng_f32, 1, 3, &state),
            PGACCEL_ERROR_INIT);
  ASSERT_EQ("resident count invalid resolution",
            pgaccel_h3_lat_lng_count_resident_bulk(&lat, &lng, &lat_f32, &lng_f32, 1, -1, &state),
            PGACCEL_ERROR_UNSUPPORTED);

  uint32_t offsets[2] = {99, 99};
  uint64_t output_cell = UINT64_MAX;
  double output_coord = 99.0;
  float polygon_coords[2] = {0.0f, 0.0f};
  uint32_t ring_offsets[2] = {0, 1};
  uint32_t ring_count = 99;

  ASSERT_EQ("grid disk size null", pgaccel_h3_grid_disk_output_size(nullptr, 1, 1, offsets),
            PGACCEL_ERROR_INIT);
  ASSERT_EQ("grid disk emit null", pgaccel_h3_grid_disk_emit(nullptr, 1, 1, offsets, &output_cell),
            PGACCEL_ERROR_INIT);
  ASSERT_STATUS_OK("grid ring size empty",
                   pgaccel_h3_grid_ring_unsafe_output_size(nullptr, 0, 1, offsets));
  ASSERT_EQ("grid ring size empty offset", offsets[0], 0u);
  ASSERT_STATUS_OK("grid ring emit empty",
                   pgaccel_h3_grid_ring_unsafe_emit(nullptr, 0, 1, nullptr, nullptr));
  ASSERT_EQ("grid ring size null", pgaccel_h3_grid_ring_unsafe_output_size(nullptr, 1, 1, offsets),
            PGACCEL_ERROR_INIT);
  ASSERT_EQ("grid ring emit null",
            pgaccel_h3_grid_ring_unsafe_emit(nullptr, 1, 1, offsets, &output_cell),
            PGACCEL_ERROR_INIT);

  ASSERT_STATUS_OK("children size empty",
                   pgaccel_h3_cell_to_children_output_size(nullptr, 0, 3, offsets));
  ASSERT_EQ("children size empty offset", offsets[0], 0u);
  ASSERT_STATUS_OK("children emit empty",
                   pgaccel_h3_cell_to_children_emit(nullptr, 0, 3, nullptr, nullptr));
  ASSERT_EQ("children size null", pgaccel_h3_cell_to_children_output_size(nullptr, 1, 3, offsets),
            PGACCEL_ERROR_INIT);
  ASSERT_EQ("children emit null",
            pgaccel_h3_cell_to_children_emit(nullptr, 1, 3, offsets, &output_cell),
            PGACCEL_ERROR_INIT);
  ASSERT_EQ("children size invalid resolution",
            pgaccel_h3_cell_to_children_output_size(&cell, 1, 16, offsets),
            PGACCEL_ERROR_UNSUPPORTED);
  offsets[0] = 0;
  offsets[1] = 1;
  ASSERT_EQ("children emit invalid resolution",
            pgaccel_h3_cell_to_children_emit(&cell, 1, -1, offsets, &output_cell),
            PGACCEL_ERROR_UNSUPPORTED);

  ASSERT_STATUS_OK("boundary size empty",
                   pgaccel_h3_cell_to_boundary_output_size(nullptr, 0, offsets));
  ASSERT_EQ("boundary size empty offset", offsets[0], 0u);
  ASSERT_STATUS_OK("boundary emit empty",
                   pgaccel_h3_cell_to_boundary_emit(nullptr, 0, nullptr, nullptr));
  ASSERT_EQ("boundary size null", pgaccel_h3_cell_to_boundary_output_size(nullptr, 1, offsets),
            PGACCEL_ERROR_INIT);
  ASSERT_EQ("boundary emit null",
            pgaccel_h3_cell_to_boundary_emit(nullptr, 1, offsets, &output_coord),
            PGACCEL_ERROR_INIT);

  ASSERT_STATUS_OK("polyfill size empty",
                   pgaccel_h3_polyfill_output_size(nullptr, nullptr, 0, 3, offsets));
  ASSERT_EQ("polyfill size empty offset", offsets[0], 0u);
  ASSERT_STATUS_OK("polyfill emit empty",
                   pgaccel_h3_polyfill_emit(nullptr, nullptr, 0, 3, nullptr, nullptr));
  ASSERT_EQ("polyfill size null",
            pgaccel_h3_polyfill_output_size(nullptr, ring_offsets, 1, 3, offsets),
            PGACCEL_ERROR_INIT);
  ASSERT_EQ("polyfill emit null",
            pgaccel_h3_polyfill_emit(polygon_coords, nullptr, 1, 3, offsets, &output_cell),
            PGACCEL_ERROR_INIT);

  ASSERT_STATUS_OK("multi polygon size empty", pgaccel_h3_cells_to_multi_polygon_output_size(
                                                   nullptr, 0, ring_offsets, &ring_count));
  ASSERT_EQ("multi polygon size empty count", ring_count, 0u);
  ASSERT_EQ("multi polygon size empty offset", ring_offsets[0], 0u);
  ASSERT_STATUS_OK("multi polygon emit empty",
                   pgaccel_h3_cells_to_multi_polygon_emit(nullptr, 0, nullptr, 0, nullptr));
  ASSERT_EQ("multi polygon size null",
            pgaccel_h3_cells_to_multi_polygon_output_size(nullptr, 1, ring_offsets, &ring_count),
            PGACCEL_ERROR_INIT);
  ASSERT_EQ("multi polygon emit null",
            pgaccel_h3_cells_to_multi_polygon_emit(nullptr, 1, ring_offsets, 1, &output_coord),
            PGACCEL_ERROR_INIT);
}

// ---------------------------------------------------------------------------
// Test: lat_lng_to_cell fp64 bulk coverage (W5 fp64-unlock plan)
//
// Exercises the soft-fp64 h3_latlng_to_cell path at 1k / 64k / 256k / 1M.
// Verifies status=OK, all cells marked valid, and all cells have the
// requested resolution. No skip-on-!fp64 branch — post fp64-unlock
// every size must run through soft-fp64 on Metal.
// ---------------------------------------------------------------------------
static void test_lat_lng_to_cell_fp64_bulk() {
  printf("--- test_lat_lng_to_cell fp64 bulk (1k/64k/256k/1M) ---\n");

  // `use_fp64=true` selects double input and therefore the fp64/soft-fp64
  // projection path. Resolution 12 keeps the high-resolution exact path hot.
  // Size list kept at 1k/64k/256k/1M per W5 fp64-unlock plan.
  std::vector<size_t> sizes = {1024, 65536, 262144, 1048576};
  for (size_t N : sizes) {
    std::vector<double> lats(N), lngs(N);
    // Spread points over a safe subset of the globe (avoid poles where
    // the ref tests say behavior is fuzzy).
    for (size_t i = 0; i < N; i++) {
      double u = (double)(i % 1001) / 1000.0;  // 0..1
      double v = (double)((i * 7919 + 13) % 1003) / 1002.0;
      lats[i] = -60.0 + u * 120.0;   // [-60, 60]
      lngs[i] = -170.0 + v * 340.0;  // [-170, 170]
    }
    std::vector<uint64_t> cells(N, 0);
    std::vector<uint8_t> valids(N, 0);
    const int resolution = 12;  // res >= 12 forces fp64 soft-path on Metal
    pgaccel_reset_gpu_exec_count();
    const uint64_t before = pgaccel_gpu_exec_count();
    pgaccel_status s = pgaccel_h3_lat_lng_to_cell_bulk(lats.data(), lngs.data(), N, resolution,
                                                       /*use_fp64=*/1, cells.data(), valids.data());
    char buf[64];
    snprintf(buf, sizeof(buf), "fp64 bulk N=%zu status", N);
    ASSERT_STATUS_OK(buf, s);
    const uint64_t after = pgaccel_gpu_exec_count();
    snprintf(buf, sizeof(buf), "fp64 bulk N=%zu launched GPU kernels", N);
    ASSERT_TRUE(buf, after > before);

    // Require high validity rate (>=99%). A few face-boundary points may
    // legitimately fail.
    size_t valid_count = 0;
    for (size_t i = 0; i < N; i++)
      valid_count += valids[i] ? 1 : 0;
    snprintf(buf, sizeof(buf), "fp64 bulk N=%zu valid_rate >= 99%%", N);
    ASSERT_TRUE(buf, valid_count * 100 >= N * 99);

    // Spot-check that valid cells have the requested resolution
    // (sample every N/64 cells — dense enough to catch systemic bugs,
    // cheap for the 1M case).
    const size_t stride = std::max<size_t>(1, N / 64);
    bool res_ok = true;
    for (size_t i = 0; i < N && res_ok; i += stride) {
      if (!valids[i])
        continue;
      int32_t r = -1;
      pgaccel_h3_get_resolution_bulk(&cells[i], 1, &r);
      if (r != resolution) {
        fprintf(stderr, "  fp64 bulk N=%zu: cells[%zu]=0x%llx has res %d, expected %d\n", N, i,
                (unsigned long long)cells[i], r, resolution);
        res_ok = false;
      }
    }
    snprintf(buf, sizeof(buf), "fp64 bulk N=%zu all sampled cells have res %d", N, resolution);
    ASSERT_TRUE(buf, res_ok);
  }
}

// ---------------------------------------------------------------------------
// Test: get_base_cell
// ---------------------------------------------------------------------------
//
// Verifies H3 v4 base-cell extraction now that the `+1` offset bug is
// fixed (commit 2026-05-01). All 122 base cells (0..121) must round-trip
// regardless of parity.
static void test_get_base_cell() {
  printf("--- test_get_base_cell ---\n");

  int digits[15] = {0};

  // Single-row sanity.
  uint64_t cell = make_cell(57, 0, digits);
  int32_t base = -42;
  pgaccel_status s = pgaccel_h3_get_base_cell_bulk(&cell, 1, &base);
  ASSERT_STATUS_OK("get_base_cell single status", s);
  ASSERT_EQ("get_base_cell single value", base, 57);

  // Bulk over a mix of even and odd bases — both must round-trip post-fix.
  const size_t N = 5;
  uint64_t cells[N];
  cells[0] = make_cell(0, 0, digits);
  cells[1] = make_cell(1, 0, digits);
  cells[2] = make_cell(14, 0, digits);
  cells[3] = make_cell(56, 0, digits);
  cells[4] = make_cell(121, 0, digits);
  int32_t bases[N] = {-1, -1, -1, -1, -1};
  s = pgaccel_h3_get_base_cell_bulk(cells, N, bases);
  ASSERT_STATUS_OK("get_base_cell bulk status", s);
  ASSERT_EQ("get_base_cell bulk[0]", bases[0], 0);
  ASSERT_EQ("get_base_cell bulk[1]", bases[1], 1);
  ASSERT_EQ("get_base_cell bulk[2]", bases[2], 14);
  ASSERT_EQ("get_base_cell bulk[3]", bases[3], 56);
  ASSERT_EQ("get_base_cell bulk[4]", bases[4], 121);

  // Sweep every base in 0..121 — full coverage of the parity dimension.
  bool sweep_ok = true;
  for (int b = 0; b < 122; b++) {
    uint64_t c = make_cell(b, 0, digits);
    int32_t out = -1;
    pgaccel_h3_get_base_cell_bulk(&c, 1, &out);
    if (out != b) {
      fprintf(stderr, "FAIL: get_base_cell sweep base=%d → got %d\n", b, out);
      sweep_ok = false;
    }
  }
  ASSERT_TRUE("get_base_cell full 0..121 sweep round-trips", sweep_ok);

  // Zero cell → kernel sentinels -1 (per FFI contract).
  uint64_t zero = 0;
  int32_t zero_base = 0;
  s = pgaccel_h3_get_base_cell_bulk(&zero, 1, &zero_base);
  ASSERT_STATUS_OK("get_base_cell zero status", s);
  ASSERT_EQ("get_base_cell zero value", zero_base, -1);
}

// ---------------------------------------------------------------------------
// Test: is_valid_cell
// ---------------------------------------------------------------------------
static void test_is_valid_cell() {
  printf("--- test_is_valid_cell ---\n");

  int digits[15] = {0};

  // Well-formed cell at res 0 (no digits, all unused = 7) is valid.
  uint64_t valid_cell = make_cell(57, 0, digits);
  uint8_t v = 99;
  pgaccel_status s = pgaccel_h3_is_valid_cell_bulk(&valid_cell, 1, &v);
  ASSERT_STATUS_OK("is_valid_cell single status", s);
  ASSERT_EQ("is_valid_cell well-formed", v, 1);

  // Zero cell is invalid.
  uint64_t zero = 0;
  v = 99;
  s = pgaccel_h3_is_valid_cell_bulk(&zero, 1, &v);
  ASSERT_STATUS_OK("is_valid_cell zero status", s);
  ASSERT_EQ("is_valid_cell zero", v, 0);

  // Mode != 1 (2 = directed edge) → invalid.
  uint64_t bad_mode = valid_cell;
  bad_mode &= ~((uint64_t)0xF << 59);  // clear mode field
  bad_mode |= ((uint64_t)2 << 59);     // set mode = 2
  v = 99;
  s = pgaccel_h3_is_valid_cell_bulk(&bad_mode, 1, &v);
  ASSERT_STATUS_OK("is_valid_cell bad-mode status", s);
  ASSERT_EQ("is_valid_cell bad mode", v, 0);

  // Base cell out of range (>121) → invalid. Use 122 (binary 1111010).
  uint64_t bad_base = make_cell(122, 0, digits);
  v = 99;
  s = pgaccel_h3_is_valid_cell_bulk(&bad_base, 1, &v);
  ASSERT_STATUS_OK("is_valid_cell bad-base status", s);
  ASSERT_EQ("is_valid_cell bad base", v, 0);

  // Every reserved-bit rule is independent. Keep them in one device batch so
  // a future validator cannot accidentally conflate the high, reserved, and
  // trailing-digit checks.
  const uint64_t bad_high_bit = valid_cell | (1ULL << 63);
  const uint64_t bad_reserved_bits = valid_cell | (1ULL << 56);
  uint64_t bad_trailing_digit = valid_cell;
  bad_trailing_digit &= ~7ULL;
  bad_trailing_digit |= 6ULL;

  // Bulk mix: valid plus every structurally invalid family.
  uint64_t cells[7] = {valid_cell,        zero, bad_mode, bad_base, bad_high_bit, bad_reserved_bits,
                       bad_trailing_digit};
  uint8_t valids[7] = {99, 99, 99, 99, 99, 99, 99};
  s = pgaccel_h3_is_valid_cell_bulk(cells, std::size(cells), valids);
  ASSERT_STATUS_OK("is_valid_cell bulk status", s);
  ASSERT_EQ("is_valid_cell bulk[0]", valids[0], 1);
  ASSERT_EQ("is_valid_cell bulk[1]", valids[1], 0);
  ASSERT_EQ("is_valid_cell bulk[2]", valids[2], 0);
  ASSERT_EQ("is_valid_cell bulk[3] bad base", valids[3], 0);
  ASSERT_EQ("is_valid_cell bulk[4] high bit", valids[4], 0);
  ASSERT_EQ("is_valid_cell bulk[5] reserved bits", valids[5], 0);
  ASSERT_EQ("is_valid_cell bulk[6] trailing digit", valids[6], 0);
}

// ---------------------------------------------------------------------------
// Test: is_pentagon
// ---------------------------------------------------------------------------
//
// Verifies pentagon classification against the canonical 12 base cells
// from the H3 v4 reference. With the +1-offset bug fixed (commit
// 2026-05-01), all 12 must classify correctly regardless of parity.
static void test_is_pentagon() {
  printf("--- test_is_pentagon ---\n");

  int digits[15] = {0};

  // The 12 pentagon base cells per the H3 v4 reference.
  static const int PENT_BASES[12] = {4, 14, 24, 38, 49, 58, 63, 72, 83, 97, 107, 117};

  // Every pentagon base at res=0 → pentagon.
  for (int i = 0; i < 12; i++) {
    uint64_t cell = make_cell(PENT_BASES[i], 0, digits);
    uint8_t v = 99;
    pgaccel_status s = pgaccel_h3_is_pentagon_bulk(&cell, 1, &v);
    char buf[80];
    snprintf(buf, sizeof(buf), "is_pentagon base=%d res=0", PENT_BASES[i]);
    ASSERT_STATUS_OK(buf, s);
    ASSERT_EQ(buf, v, 1);
  }

  // Hexagon bases (mix of even/odd) are NOT pentagons.
  static const int HEX_BASES[4] = {0, 1, 56, 57};
  for (int i = 0; i < 4; i++) {
    uint64_t cell = make_cell(HEX_BASES[i], 0, digits);
    uint8_t v = 99;
    pgaccel_status s = pgaccel_h3_is_pentagon_bulk(&cell, 1, &v);
    char buf[80];
    snprintf(buf, sizeof(buf), "is_pentagon hex base=%d res=0", HEX_BASES[i]);
    ASSERT_STATUS_OK(buf, s);
    ASSERT_EQ(buf, v, 0);
  }

  // Pentagon base + all-zero sub-resolution digits → still pentagon.
  int zero_digits[15] = {0};
  uint64_t cell_p_r3 = make_cell(4, 3, zero_digits);
  uint8_t v = 99;
  pgaccel_status s = pgaccel_h3_is_pentagon_bulk(&cell_p_r3, 1, &v);
  ASSERT_STATUS_OK("is_pentagon base=4 res=3 all-zero digits status", s);
  ASSERT_EQ("is_pentagon base=4 res=3 all-zero digits", v, 1);

  // Pentagon base + non-zero leading digit → NOT pentagon.
  int mixed_digits[15] = {1, 0, 0};
  uint64_t cell_p_mixed = make_cell(4, 3, mixed_digits);
  v = 99;
  s = pgaccel_h3_is_pentagon_bulk(&cell_p_mixed, 1, &v);
  ASSERT_STATUS_OK("is_pentagon base=4 res=3 mixed-digit status", s);
  ASSERT_EQ("is_pentagon base=4 res=3 mixed-digit", v, 0);

  // Zero cell → not a pentagon.
  uint64_t zero = 0;
  v = 99;
  s = pgaccel_h3_is_pentagon_bulk(&zero, 1, &v);
  ASSERT_STATUS_OK("is_pentagon zero status", s);
  ASSERT_EQ("is_pentagon zero value", v, 0);
}

// ---------------------------------------------------------------------------
// Test: is_res_class_iii
// ---------------------------------------------------------------------------
static void test_is_res_class_iii() {
  printf("--- test_is_res_class_iii ---\n");

  int digits[15] = {0};

  // Class III iff resolution is odd. Sweep res 0..15.
  for (int r = 0; r <= 15; r++) {
    uint64_t cell = make_cell(57, r, digits);
    uint8_t v = 99;
    pgaccel_status s = pgaccel_h3_is_res_class_iii_bulk(&cell, 1, &v);
    char buf[64];
    snprintf(buf, sizeof(buf), "is_res_class_iii res=%d status", r);
    ASSERT_STATUS_OK(buf, s);
    snprintf(buf, sizeof(buf), "is_res_class_iii res=%d value", r);
    ASSERT_EQ(buf, v, (uint8_t)(r & 1));
  }

  // Bulk: alternating res 0..3 → expect [0, 1, 0, 1].
  uint64_t cells[4] = {make_cell(57, 0, digits), make_cell(57, 1, digits), make_cell(57, 2, digits),
                       make_cell(57, 3, digits)};
  uint8_t out[4] = {99, 99, 99, 99};
  pgaccel_status s = pgaccel_h3_is_res_class_iii_bulk(cells, 4, out);
  ASSERT_STATUS_OK("is_res_class_iii bulk status", s);
  ASSERT_EQ("is_res_class_iii bulk[0]", out[0], 0);
  ASSERT_EQ("is_res_class_iii bulk[1]", out[1], 1);
  ASSERT_EQ("is_res_class_iii bulk[2]", out[2], 0);
  ASSERT_EQ("is_res_class_iii bulk[3]", out[3], 1);
}

// ---------------------------------------------------------------------------
// Test: cell_to_center_child
// ---------------------------------------------------------------------------
static void test_cell_to_center_child() {
  printf("--- test_cell_to_center_child ---\n");

  int digits[15] = {0};

  // Same-resolution → returns input cell unchanged.
  uint64_t parent = make_cell(57, 3, digits);
  uint64_t child = 0;
  pgaccel_status s = pgaccel_h3_cell_to_center_child_bulk(&parent, 1, 3, &child);
  ASSERT_STATUS_OK("center_child same-res status", s);
  ASSERT_TRUE("center_child same-res returns input", child == parent);

  // Descend from res 0 to res 2 → digits [0, 0] populated, base preserved
  // (post-fix; the prior +1-offset layout would have stripped the LSB of
  // an odd base on descent — see commit 2026-05-01). Use base 57 (odd)
  // to exercise the fixed bit-45 boundary.
  uint64_t cell_r0 = make_cell(57, 0, digits);
  uint64_t cell_r2 = 0;
  s = pgaccel_h3_cell_to_center_child_bulk(&cell_r0, 1, 2, &cell_r2);
  ASSERT_STATUS_OK("center_child r0->r2 status", s);
  int32_t res_out = -1;
  pgaccel_h3_get_resolution_bulk(&cell_r2, 1, &res_out);
  ASSERT_EQ("center_child r0->r2 has res 2", res_out, 2);
  int32_t base_out = -1;
  pgaccel_h3_get_base_cell_bulk(&cell_r2, 1, &base_out);
  ASSERT_EQ("center_child r0->r2 base preserved (odd base)", base_out, 57);

  // Invalid: child_res < cell.res → 0.
  s = pgaccel_h3_cell_to_center_child_bulk(&parent, 1, 1, &child);
  ASSERT_STATUS_OK("center_child invalid child_res status", s);
  ASSERT_TRUE("center_child invalid child_res returns 0", child == 0);

  // Out-of-range child_res → kernel returns OK but writes 0.
  s = pgaccel_h3_cell_to_center_child_bulk(&cell_r0, 1, 16, &child);
  ASSERT_TRUE("center_child child_res=16 returns 0", child == 0);

  // Zero cell → 0.
  uint64_t zero = 0;
  s = pgaccel_h3_cell_to_center_child_bulk(&zero, 1, 5, &child);
  ASSERT_STATUS_OK("center_child zero-input status", s);
  ASSERT_TRUE("center_child zero-input returns 0", child == 0);
}

// ---------------------------------------------------------------------------
// Variable-output kernel tests
// ---------------------------------------------------------------------------

// Build a known-pentagon cell at the given resolution. Pentagon base 4 with
// all-zero digits is a valid pentagon per the H3 layout convention used by
// pgaccel_h3_is_pentagon_bulk.
static uint64_t make_pentagon_cell(int resolution) {
  int digits[15] = {0};
  return make_cell(4, resolution, digits);
}

// Exact neighbor traversal is not implemented on device. Both passes must
// fail closed without mutating caller buffers or claiming a GPU dispatch.
static void test_grid_disk() {
  printf("--- test_grid_disk ---\n");

  int digits[15] = {0};
  uint64_t cells[2] = {make_cell(57, 5, digits), make_pentagon_cell(5)};
  uint32_t offsets[3] = {0xA5A5A5A5u, 0xA5A5A5A5u, 0xA5A5A5A5u};
  uint64_t output[3] = {0xDEADBEEF, 0xDEADBEEF, 0xDEADBEEF};

  pgaccel_reset_gpu_exec_count();
  ASSERT_EQ("grid_disk size fails closed", pgaccel_h3_grid_disk_output_size(cells, 2, 1, offsets),
            PGACCEL_ERROR_UNSUPPORTED);
  ASSERT_EQ("grid_disk size leaves offset[0] untouched", offsets[0], 0xA5A5A5A5u);
  ASSERT_EQ("grid_disk size leaves offset[2] untouched", offsets[2], 0xA5A5A5A5u);
  ASSERT_EQ("grid_disk emit fails closed", pgaccel_h3_grid_disk_emit(cells, 2, 1, offsets, output),
            PGACCEL_ERROR_UNSUPPORTED);
  ASSERT_EQ("grid_disk emit leaves output untouched", output[0], 0xDEADBEEFu);
  ASSERT_EQ("grid_disk quarantine records zero GPU dispatches", pgaccel_gpu_exec_count(), 0u);

  uint32_t empty_offset = 99;
  ASSERT_STATUS_OK("grid_disk empty size remains valid",
                   pgaccel_h3_grid_disk_output_size(nullptr, 0, 1, &empty_offset));
  ASSERT_EQ("grid_disk empty size writes zero", empty_offset, 0u);
  ASSERT_STATUS_OK("grid_disk empty emit remains valid",
                   pgaccel_h3_grid_disk_emit(nullptr, 0, 1, nullptr, nullptr));
}

// Ring traversal has the same fail-closed contract as grid_disk.
static void test_grid_ring_unsafe() {
  printf("--- test_grid_ring_unsafe ---\n");

  int digits[15] = {0};
  uint64_t cell = make_cell(57, 5, digits);
  uint32_t offsets[2] = {0xA5A5A5A5u, 0xA5A5A5A5u};
  uint64_t output = 0xDEADBEEF;

  pgaccel_reset_gpu_exec_count();
  ASSERT_EQ("grid_ring size fails closed",
            pgaccel_h3_grid_ring_unsafe_output_size(&cell, 1, 1, offsets),
            PGACCEL_ERROR_UNSUPPORTED);
  ASSERT_EQ("grid_ring size leaves offsets untouched", offsets[0], 0xA5A5A5A5u);
  ASSERT_EQ("grid_ring emit fails closed",
            pgaccel_h3_grid_ring_unsafe_emit(&cell, 1, 1, offsets, &output),
            PGACCEL_ERROR_UNSUPPORTED);
  ASSERT_EQ("grid_ring emit leaves output untouched", output, 0xDEADBEEFu);
  ASSERT_EQ("grid_ring quarantine records zero GPU dispatches", pgaccel_gpu_exec_count(), 0u);
}

// Test: cell_to_children — count formula and same-res passthrough.
static void test_cell_to_children() {
  printf("--- test_cell_to_children ---\n");

  int digits[15] = {0};
  uint64_t parent_r3 = make_cell(57, 3, digits);

  // child_res == cell.res → 1 cell (the input itself).
  {
    uint32_t off[2] = {99, 99};
    pgaccel_status s = pgaccel_h3_cell_to_children_output_size(&parent_r3, 1, 3, off);
    ASSERT_STATUS_OK("c2c same-res size status", s);
    ASSERT_EQ("c2c same-res count", off[1], 1);

    uint64_t out[1] = {0};
    s = pgaccel_h3_cell_to_children_emit(&parent_r3, 1, 3, off, out);
    ASSERT_STATUS_OK("c2c same-res emit status", s);
    ASSERT_TRUE("c2c same-res returns input", out[0] == parent_r3);
  }

  // child_res = res + 1 → 7 children for hexagon.
  {
    uint32_t off[2] = {99, 99};
    pgaccel_status s = pgaccel_h3_cell_to_children_output_size(&parent_r3, 1, 4, off);
    ASSERT_STATUS_OK("c2c r4 size status", s);
    ASSERT_EQ("c2c r4 hex count", off[1], 7);

    std::vector<uint64_t> out(7, 0);
    s = pgaccel_h3_cell_to_children_emit(&parent_r3, 1, 4, off, out.data());
    ASSERT_STATUS_OK("c2c r4 emit status", s);
    // All children should be non-zero, distinct, at resolution 4.
    bool distinct = true;
    for (size_t i = 0; i < 7; i++) {
      if (out[i] == 0) {
        distinct = false;
        break;
      }
      for (size_t j = i + 1; j < 7; j++) {
        if (out[i] == out[j]) {
          distinct = false;
          break;
        }
      }
    }
    ASSERT_TRUE("c2c r4 children distinct + non-zero", distinct);

    // Verify resolution
    int32_t child_res[7] = {-1, -1, -1, -1, -1, -1, -1};
    pgaccel_h3_get_resolution_bulk(out.data(), 7, child_res);
    bool all_r4 = true;
    for (int i = 0; i < 7; i++) {
      if (child_res[i] != 4) {
        all_r4 = false;
        break;
      }
    }
    ASSERT_TRUE("c2c r4 all children at res 4", all_r4);
  }

  // child_res = res + 2 → 49 children (7^2) for hexagon.
  {
    uint32_t off[2] = {99, 99};
    pgaccel_status s = pgaccel_h3_cell_to_children_output_size(&parent_r3, 1, 5, off);
    ASSERT_STATUS_OK("c2c r5 size status", s);
    ASSERT_EQ("c2c r5 hex count", off[1], 49);
  }

  // child_res = res + 3 exercises the nested base-7 divisor used to decode
  // every digit after the leading child digit.
  {
    uint32_t off[2] = {99, 99};
    pgaccel_status s = pgaccel_h3_cell_to_children_output_size(&parent_r3, 1, 6, off);
    ASSERT_STATUS_OK("c2c r6 size status", s);
    ASSERT_EQ("c2c r6 hex count", off[1], 343);

    std::vector<uint64_t> out(off[1], 0);
    s = pgaccel_h3_cell_to_children_emit(&parent_r3, 1, 6, off, out.data());
    ASSERT_STATUS_OK("c2c r6 emit status", s);
    int last_digits[15] = {0, 0, 0, 6, 6, 6, 0, 0, 0, 0, 0, 0, 0, 0, 0};
    ASSERT_EQ("c2c r6 first child", out.front(), make_cell(57, 6, digits));
    ASSERT_EQ("c2c r6 last child", out.back(), make_cell(57, 6, last_digits));
    std::sort(out.begin(), out.end());
    ASSERT_TRUE("c2c r6 children distinct",
                std::adjacent_find(out.begin(), out.end()) == out.end());
  }

  // Pentagon: child_res = res + 1 → 5 children (pentagon has 5 not 7).
  {
    uint64_t pent = make_pentagon_cell(3);
    uint32_t off[2] = {99, 99};
    pgaccel_status s = pgaccel_h3_cell_to_children_output_size(&pent, 1, 4, off);
    ASSERT_STATUS_OK("c2c pent r4 size status", s);
    ASSERT_EQ("c2c pent r4 count", off[1], 5);
  }

  // Deep expansion exercises the multi-digit encoder for both low and high
  // pentagon base-cell masks. An off-centre descendant of a pentagon base is
  // a hexagon and therefore retains the full 7^delta fan-out.
  {
    int off_center_digits[15] = {2, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0};
    uint64_t parents[3] = {make_pentagon_cell(3), make_cell(72, 3, digits),
                           make_cell(72, 3, off_center_digits)};
    uint32_t off[4] = {99, 99, 99, 99};
    pgaccel_status s = pgaccel_h3_cell_to_children_output_size(parents, 3, 5, off);
    ASSERT_STATUS_OK("c2c deep pentagon size status", s);
    ASSERT_EQ("c2c deep low pentagon count", off[1] - off[0], 35);
    ASSERT_EQ("c2c deep high pentagon count", off[2] - off[1], 35);
    ASSERT_EQ("c2c deep off-centre pentagon-base count", off[3] - off[2], 49);

    std::vector<uint64_t> out(off[3], 0);
    pgaccel_reset_gpu_exec_count();
    s = pgaccel_h3_cell_to_children_emit(parents, 3, 5, off, out.data());
    ASSERT_STATUS_OK("c2c deep pentagon emit status", s);
    ASSERT_TRUE("c2c deep pentagon emit dispatched", pgaccel_gpu_exec_count() > 0);

    bool rows_valid = true;
    for (size_t row = 0; row < 3; ++row) {
      std::vector<uint64_t> row_cells(out.begin() + off[row], out.begin() + off[row + 1]);
      std::sort(row_cells.begin(), row_cells.end());
      rows_valid = rows_valid && !row_cells.empty() && row_cells.front() != 0 &&
                   std::adjacent_find(row_cells.begin(), row_cells.end()) == row_cells.end();
      std::vector<int32_t> child_resolutions(row_cells.size(), -1);
      s = pgaccel_h3_get_resolution_bulk(row_cells.data(), row_cells.size(),
                                         child_resolutions.data());
      rows_valid = rows_valid && s == PGACCEL_OK &&
                   std::all_of(child_resolutions.begin(), child_resolutions.end(),
                               [](int32_t value) { return value == 5; });
    }
    ASSERT_TRUE("c2c deep rows are distinct resolution-5 cells", rows_valid);
  }

  // Mixed rows exercise the shared-slab layout used by the size and emit
  // kernels: cells[count], offsets[count+1], then aligned output.
  {
    uint64_t cells[3] = {parent_r3, make_pentagon_cell(3), 0};
    uint32_t off[4] = {99, 99, 99, 99};
    pgaccel_status s = pgaccel_h3_cell_to_children_output_size(cells, 3, 4, off);
    ASSERT_STATUS_OK("c2c mixed size status", s);
    ASSERT_EQ("c2c mixed off[0]", off[0], 0);
    ASSERT_EQ("c2c mixed off[1]", off[1], 7);
    ASSERT_EQ("c2c mixed off[2]", off[2], 12);
    ASSERT_EQ("c2c mixed off[3]", off[3], 12);

    std::vector<uint64_t> out(off[3], 0);
    s = pgaccel_h3_cell_to_children_emit(cells, 3, 4, off, out.data());
    ASSERT_STATUS_OK("c2c mixed emit status", s);

    bool wrote_expected_rows = true;
    for (uint32_t i = off[0]; i < off[2]; i++) {
      if (out[i] == 0) {
        wrote_expected_rows = false;
        break;
      }
    }
    ASSERT_TRUE("c2c mixed emit writes non-empty rows", wrote_expected_rows);
  }

  // Three valid 7^11 fan-outs exceed the uint32 offset ABI even though each
  // individual row count fits. The size pass must reject the prefix sum before
  // copying partial offsets to the caller.
  {
    uint64_t wide_parents[3] = {make_cell(57, 0, digits), make_cell(57, 0, digits),
                                make_cell(57, 0, digits)};
    uint32_t off[4] = {0xA5A5A5A5u, 0xA5A5A5A5u, 0xA5A5A5A5u, 0xA5A5A5A5u};
    const pgaccel_status s = pgaccel_h3_cell_to_children_output_size(wide_parents, 3, 11, off);
    ASSERT_EQ("c2c uint32 prefix overflow rejected", s, PGACCEL_ERROR_UNSUPPORTED);
    ASSERT_TRUE("c2c overflow leaves caller offsets untouched",
                std::all_of(std::begin(off), std::end(off),
                            [](uint32_t value) { return value == 0xA5A5A5A5u; }));
  }

  // Invalid: child_res < cell.res → 0 cells.
  {
    uint32_t off[2] = {99, 99};
    pgaccel_status s = pgaccel_h3_cell_to_children_output_size(&parent_r3, 1, 2, off);
    ASSERT_STATUS_OK("c2c invalid size status", s);
    ASSERT_EQ("c2c invalid count", off[1], 0);

    uint64_t untouched = 0xDEADBEEFCAFEBABEULL;
    pgaccel_reset_gpu_exec_count();
    s = pgaccel_h3_cell_to_children_emit(&parent_r3, 1, 2, off, &untouched);
    ASSERT_STATUS_OK("c2c zero-output emit status", s);
    ASSERT_TRUE("c2c zero-output emit preserves output", untouched == 0xDEADBEEFCAFEBABEULL);
    ASSERT_EQ("c2c zero-output emit launches no GPU work", pgaccel_gpu_exec_count(), 0u);
  }

  // Defensive emit branches remain deterministic even if a caller supplies
  // offsets that did not come from the size pass.
  {
    uint64_t defensive_cells[2] = {0, parent_r3};
    uint32_t defensive_offsets[3] = {0, 1, 2};
    uint64_t defensive_output[2] = {UINT64_MAX, UINT64_MAX};
    const pgaccel_status s = pgaccel_h3_cell_to_children_emit(defensive_cells, 2, 2,
                                                              defensive_offsets, defensive_output);
    ASSERT_STATUS_OK("c2c defensive emit status", s);
    ASSERT_EQ("c2c defensive zero cell emits zero", defensive_output[0], 0ULL);
    ASSERT_EQ("c2c defensive coarser target emits zero", defensive_output[1], 0ULL);
  }
}

// Exact boundary geometry is unavailable on device and must fail closed.
static void test_cell_to_boundary() {
  printf("--- test_cell_to_boundary ---\n");

  int digits[15] = {0};
  uint64_t cells[2] = {make_cell(5, 3, digits), make_pentagon_cell(3)};
  uint32_t offsets[3] = {0xA5A5A5A5u, 0xA5A5A5A5u, 0xA5A5A5A5u};
  double output[2] = {123.5, -456.25};

  pgaccel_reset_gpu_exec_count();
  ASSERT_EQ("boundary size fails closed",
            pgaccel_h3_cell_to_boundary_output_size(cells, 2, offsets), PGACCEL_ERROR_UNSUPPORTED);
  ASSERT_EQ("boundary size leaves offsets untouched", offsets[0], 0xA5A5A5A5u);
  ASSERT_EQ("boundary emit fails closed",
            pgaccel_h3_cell_to_boundary_emit(cells, 2, offsets, output), PGACCEL_ERROR_UNSUPPORTED);
  ASSERT_TRUE("boundary emit leaves coordinates untouched",
              output[0] == 123.5 && output[1] == -456.25);
  ASSERT_EQ("boundary quarantine records zero GPU dispatches", pgaccel_gpu_exec_count(), 0u);
}

// Exact polygon containment is unavailable on device and must fail closed.
static void test_polyfill() {
  printf("--- test_polyfill ---\n");

  float coords[] = {-25.0f, -25.0f, 25.0f, -25.0f, 25.0f, 25.0f, -25.0f, 25.0f, -25.0f, -25.0f};
  uint32_t ring_offsets[2] = {0, 5};
  uint32_t out_offsets[2] = {0xA5A5A5A5u, 0xA5A5A5A5u};
  uint64_t output = 0xDEADBEEF;

  pgaccel_reset_gpu_exec_count();
  ASSERT_EQ("polyfill size fails closed",
            pgaccel_h3_polyfill_output_size(coords, ring_offsets, 1, 4, out_offsets),
            PGACCEL_ERROR_UNSUPPORTED);
  ASSERT_EQ("polyfill size leaves offsets untouched", out_offsets[0], 0xA5A5A5A5u);
  ASSERT_EQ("polyfill emit fails closed",
            pgaccel_h3_polyfill_emit(coords, ring_offsets, 1, 4, out_offsets, &output),
            PGACCEL_ERROR_UNSUPPORTED);
  ASSERT_EQ("polyfill emit leaves output untouched", output, 0xDEADBEEFu);
  ASSERT_EQ("polyfill quarantine records zero GPU dispatches", pgaccel_gpu_exec_count(), 0u);
}

// Exact union topology is unavailable on device and must fail closed.
static void test_cells_to_multi_polygon() {
  printf("--- test_cells_to_multi_polygon ---\n");

  int digits[15] = {0};
  uint64_t cells[2] = {make_cell(5, 3, digits), make_cell(6, 3, digits)};
  uint32_t ring_offsets[3] = {0xA5A5A5A5u, 0xA5A5A5A5u, 0xA5A5A5A5u};
  uint32_t ring_count = 0xA5A5A5A5u;
  double output = 123.5;

  pgaccel_reset_gpu_exec_count();
  ASSERT_EQ("multi_polygon size fails closed",
            pgaccel_h3_cells_to_multi_polygon_output_size(cells, 2, ring_offsets, &ring_count),
            PGACCEL_ERROR_UNSUPPORTED);
  ASSERT_EQ("multi_polygon size leaves ring count untouched", ring_count, 0xA5A5A5A5u);
  ASSERT_EQ("multi_polygon size leaves offsets untouched", ring_offsets[0], 0xA5A5A5A5u);
  ASSERT_EQ("multi_polygon emit fails closed",
            pgaccel_h3_cells_to_multi_polygon_emit(cells, 2, ring_offsets, 2, &output),
            PGACCEL_ERROR_UNSUPPORTED);
  ASSERT_TRUE("multi_polygon emit leaves output untouched", output == 123.5);
  ASSERT_EQ("multi_polygon quarantine records zero GPU dispatches", pgaccel_gpu_exec_count(), 0u);
}

static void test_executed_h3_branch_matrix() {
  printf("--- test_executed_h3_branch_matrix ---\n");

  static const int pentagon_bases[] = {4, 14, 24, 38, 49, 58, 63, 72, 83, 97, 107, 117};
  const auto is_pentagon_base = [](int base) {
    static const int values[] = {4, 14, 24, 38, 49, 58, 63, 72, 83, 97, 107, 117};
    return std::find(std::begin(values), std::end(values), base) != std::end(values);
  };

  // One dispatch per operation covers both base-cell mask halves, every
  // resolution parity, pentagons, and ordinary hexagons.
  std::vector<uint64_t> sweep_cells;
  for (int base = 0; base < 122; ++base) {
    for (int resolution = 0; resolution <= 15; ++resolution) {
      int digits[15] = {0};
      sweep_cells.push_back(make_cell(base, resolution, digits));
    }
  }
  std::vector<int32_t> resolutions(sweep_cells.size(), -1);
  std::vector<int32_t> bases(sweep_cells.size(), -1);
  std::vector<uint8_t> valid(sweep_cells.size(), 99);
  std::vector<uint8_t> pentagon(sweep_cells.size(), 99);
  std::vector<uint8_t> class_iii(sweep_cells.size(), 99);
  pgaccel_reset_gpu_exec_count();
  ASSERT_STATUS_OK(
      "H3 sweep resolution status",
      pgaccel_h3_get_resolution_bulk(sweep_cells.data(), sweep_cells.size(), resolutions.data()));
  ASSERT_STATUS_OK(
      "H3 sweep base status",
      pgaccel_h3_get_base_cell_bulk(sweep_cells.data(), sweep_cells.size(), bases.data()));
  ASSERT_STATUS_OK(
      "H3 sweep validity status",
      pgaccel_h3_is_valid_cell_bulk(sweep_cells.data(), sweep_cells.size(), valid.data()));
  ASSERT_STATUS_OK(
      "H3 sweep pentagon status",
      pgaccel_h3_is_pentagon_bulk(sweep_cells.data(), sweep_cells.size(), pentagon.data()));
  ASSERT_STATUS_OK(
      "H3 sweep class III status",
      pgaccel_h3_is_res_class_iii_bulk(sweep_cells.data(), sweep_cells.size(), class_iii.data()));
  bool sweep_ok = true;
  for (int base = 0; base < 122; ++base) {
    for (int resolution = 0; resolution <= 15; ++resolution) {
      const size_t i = static_cast<size_t>(base * 16 + resolution);
      sweep_ok = sweep_ok && resolutions[i] == resolution && bases[i] == base && valid[i] == 1 &&
                 pentagon[i] == static_cast<uint8_t>(is_pentagon_base(base)) &&
                 class_iii[i] == static_cast<uint8_t>(resolution & 1);
    }
  }
  ASSERT_TRUE("H3 all-base/all-resolution sweep values", sweep_ok);
  ASSERT_TRUE("H3 all-base/all-resolution sweep dispatched", pgaccel_gpu_exec_count() >= 5);

  // Exercise every branch of the stricter validator used by the fused parent
  // count kernel. All rows run even though the aggregate call rejects the
  // batch after the validation pass.
  int zero_digits[15] = {0};
  const uint64_t ordinary = make_cell(57, 3, zero_digits);
  int active_unused_digits[15] = {7, 0, 0};
  int deleted_k_digits[15] = {1, 0, 0};
  int deleted_k_late_digits[15] = {0, 1, 0};
  uint64_t bad_trailing = ordinary & ~UINT64_C(7);
  std::vector<uint64_t> malformed = {
      0,
      ordinary | (UINT64_C(1) << 63),
      ordinary | (UINT64_C(1) << 56),
      ordinary | (UINT64_C(1) << 57),
      ordinary | (UINT64_C(1) << 58),
      (ordinary & ~(UINT64_C(0xf) << 59)) | (UINT64_C(2) << 59),
      (ordinary & ~(UINT64_C(0xf) << 59)) | (UINT64_C(15) << 59),
      make_cell(122, 3, zero_digits),
      make_cell(127, 3, zero_digits),
      make_cell(57, 3, active_unused_digits),
      bad_trailing,
      make_cell(4, 3, deleted_k_digits),
      make_cell(72, 3, deleted_k_late_digits),
  };
  pgaccel_agg_state* state = reinterpret_cast<pgaccel_agg_state*>(uintptr_t{1});
  pgaccel_reset_gpu_exec_count();
  ASSERT_EQ("parent count malformed matrix rejected",
            pgaccel_h3_cell_to_parent_count_bulk(malformed.data(), malformed.size(), 0, &state),
            PGACCEL_ERROR);
  ASSERT_TRUE("parent count malformed matrix clears state", state == nullptr);
  ASSERT_TRUE("parent count malformed matrix dispatched", pgaccel_gpu_exec_count() > 0);

  // Valid active digits cover both identity and masking paths, all digit
  // directions, and both halves of the pentagon membership test.
  std::vector<uint64_t> parent_count_cells;
  for (int base = 0; base < 122; ++base) {
    parent_count_cells.push_back(make_cell(base, 0, zero_digits));
    for (int digit = 0; digit <= 6; ++digit) {
      if (digit == 1 && is_pentagon_base(base))
        continue;
      int digits[15];
      std::fill(std::begin(digits), std::end(digits), digit);
      parent_count_cells.push_back(make_cell(base, 15, digits));
    }
  }
  state = nullptr;
  pgaccel_reset_gpu_exec_count();
  ASSERT_STATUS_OK("parent count valid semantic matrix status",
                   pgaccel_h3_cell_to_parent_count_bulk(parent_count_cells.data(),
                                                        parent_count_cells.size(), 0, &state));
  ASSERT_TRUE("parent count valid semantic matrix state", state != nullptr);
  ASSERT_TRUE("parent count valid semantic matrix dispatched", pgaccel_gpu_exec_count() > 0);
  pgaccel_agg_free(state);

  // Deep same-base paths exercise all IJK directions repeatedly rather than
  // only at resolution one.
  std::vector<uint64_t> direction_cells;
  for (int digit = 0; digit <= 6; ++digit) {
    int digits[15];
    std::fill(std::begin(digits), std::end(digits), digit);
    direction_cells.push_back(make_cell(57, 15, digits));
  }
  std::vector<uint64_t> distance_a;
  std::vector<uint64_t> distance_b;
  for (uint64_t a : direction_cells) {
    for (uint64_t b : direction_cells) {
      distance_a.push_back(a);
      distance_b.push_back(b);
    }
  }
  std::vector<int32_t> distances(distance_a.size(), -1);
  ASSERT_STATUS_OK("deep direction distance matrix status",
                   pgaccel_h3_grid_distance_bulk(distance_a.data(), distance_b.data(),
                                                 distance_a.size(), distances.data()));
  bool distances_ok = true;
  for (size_t a = 0; a < direction_cells.size(); ++a) {
    for (size_t b = 0; b < direction_cells.size(); ++b) {
      const int32_t ab = distances[a * direction_cells.size() + b];
      const int32_t ba = distances[b * direction_cells.size() + a];
      distances_ok = distances_ok && ab >= 0 && ab == ba && (a != b || ab == 0);
    }
  }
  ASSERT_TRUE("deep direction distance matrix symmetric", distances_ok);

  // Drive every exact coordinate rejection arm in the same kernels that also
  // process valid rows.
  const double nan = std::numeric_limits<double>::quiet_NaN();
  const double infinity = std::numeric_limits<double>::infinity();
  const double lats[] = {-90.001, 90.001, 0.0, 0.0, nan, infinity, -infinity, 0.0, 0.0, 45.0};
  const double lngs[] = {0.0, 0.0, -180.001, 180.001, 0.0, 0.0, 0.0, nan, infinity, -73.0};
  constexpr size_t coordinate_count = std::size(lats);
  uint64_t cells[coordinate_count];
  uint8_t coordinate_valid[coordinate_count];
  for (int resolution : {0, 7, 15}) {
    std::fill(std::begin(cells), std::end(cells), UINT64_MAX);
    std::fill(std::begin(coordinate_valid), std::end(coordinate_valid), uint8_t{99});
    ASSERT_STATUS_OK("exact coordinate semantic matrix status",
                     pgaccel_h3_lat_lng_to_cell_bulk(lats, lngs, coordinate_count, resolution,
                                                     /*use_fp64=*/1, cells, coordinate_valid));
    bool rejection_ok =
        coordinate_valid[coordinate_count - 1] == 1 && cells[coordinate_count - 1] != 0;
    for (size_t i = 0; i + 1 < coordinate_count; ++i)
      rejection_ok = rejection_ok && coordinate_valid[i] == 0 && cells[i] == 0;
    ASSERT_TRUE("exact coordinate semantic matrix values", rejection_ok);
  }

  ASSERT_TRUE("pentagon fixture list remains complete", std::size(pentagon_bases) == 12);
}

static void test_no_device_paths() {
  printf("--- H3 APIs: no-device lifecycle ---\n");

  const pgaccel_status init_status = pgaccel_init();
  ASSERT_TRUE("no-device initialization rejected", init_status != PGACCEL_OK);
  if (init_status == PGACCEL_OK) {
    ASSERT_STATUS_OK("unexpected no-device initialization shuts down", pgaccel_shutdown());
    return;
  }

  int digits[15] = {0};
  const uint64_t cell = make_cell(57, 3, digits);
  int32_t integer_output = -1;
  int32_t distance = -1;
  uint8_t boolean_output = 99;
  uint64_t cell_output = 0;
  double lat = 37.7749;
  double lng = -122.4194;
  float lat_f32 = static_cast<float>(lat);
  float lng_f32 = static_cast<float>(lng);
  pgaccel_agg_state* state = reinterpret_cast<pgaccel_agg_state*>(uintptr_t{1});

  ASSERT_EQ("resolution reports no device",
            pgaccel_h3_get_resolution_bulk(&cell, 1, &integer_output), PGACCEL_ERROR_NO_DEVICE);
  ASSERT_EQ("base cell reports no device", pgaccel_h3_get_base_cell_bulk(&cell, 1, &integer_output),
            PGACCEL_ERROR_NO_DEVICE);
  ASSERT_EQ("validity reports no device", pgaccel_h3_is_valid_cell_bulk(&cell, 1, &boolean_output),
            PGACCEL_ERROR_NO_DEVICE);
  ASSERT_EQ("pentagon reports no device", pgaccel_h3_is_pentagon_bulk(&cell, 1, &boolean_output),
            PGACCEL_ERROR_NO_DEVICE);
  ASSERT_EQ("resolution class reports no device",
            pgaccel_h3_is_res_class_iii_bulk(&cell, 1, &boolean_output), PGACCEL_ERROR_NO_DEVICE);
  ASSERT_EQ("parent reports no device", pgaccel_h3_cell_to_parent_bulk(&cell, 1, 2, &cell_output),
            PGACCEL_ERROR_NO_DEVICE);

  int32_t detail = PGACCEL_H3_PARENT_DETAIL_CONTRACT;
  ASSERT_EQ("resident parent reports no device",
            pgaccel_h3_cell_to_parent_resident_ex(&cell, nullptr, 1, 2, &cell_output, &detail),
            PGACCEL_ERROR_NO_DEVICE);
  ASSERT_EQ("resident parent leaves no failure detail", detail, PGACCEL_H3_PARENT_DETAIL_NONE);
  ASSERT_EQ("parent count reports no device",
            pgaccel_h3_cell_to_parent_count_bulk(&cell, 1, 2, &state), PGACCEL_ERROR_NO_DEVICE);
  ASSERT_TRUE("parent count clears state without device", state == nullptr);
  ASSERT_EQ("center child reports no device",
            pgaccel_h3_cell_to_center_child_bulk(&cell, 1, 4, &cell_output),
            PGACCEL_ERROR_NO_DEVICE);
  ASSERT_EQ("grid distance reports no device",
            pgaccel_h3_grid_distance_bulk(&cell, &cell, 1, &distance), PGACCEL_ERROR_NO_DEVICE);
  ASSERT_EQ("lat/lng conversion reports no device",
            pgaccel_h3_lat_lng_to_cell_bulk(&lat, &lng, 1, 3, true, &cell_output, &boolean_output),
            PGACCEL_ERROR_NO_DEVICE);

  state = reinterpret_cast<pgaccel_agg_state*>(uintptr_t{1});
  ASSERT_EQ("lat/lng count reports no device",
            pgaccel_h3_lat_lng_count_bulk(&lat, &lng, 1, 3, &state), PGACCEL_ERROR_NO_DEVICE);
  ASSERT_TRUE("lat/lng count clears state without device", state == nullptr);
  state = reinterpret_cast<pgaccel_agg_state*>(uintptr_t{1});
  ASSERT_EQ("resident lat/lng count reports no device",
            pgaccel_h3_lat_lng_count_resident_bulk(&lat, &lng, &lat_f32, &lng_f32, 1, 3, &state),
            PGACCEL_ERROR_NO_DEVICE);
  ASSERT_TRUE("resident lat/lng count clears state without device", state == nullptr);

  uint32_t offsets[2] = {0, 1};
  ASSERT_EQ("children size reports no device",
            pgaccel_h3_cell_to_children_output_size(&cell, 1, 4, offsets), PGACCEL_ERROR_NO_DEVICE);
  ASSERT_EQ("children emit reports no device",
            pgaccel_h3_cell_to_children_emit(&cell, 1, 4, offsets, &cell_output),
            PGACCEL_ERROR_NO_DEVICE);

  ASSERT_STATUS_OK("failed no-device initialization shuts down", pgaccel_shutdown());
}

static bool run_no_device_child(const char* executable) {
  const pid_t child = fork();
  if (child < 0) {
    std::fprintf(stderr, "FAIL fork no-device H3 matrix: errno=%d\n", errno);
    return false;
  }
  if (child == 0) {
    const char* visibility_mask = std::getenv("PGACCEL_TEST_NO_DEVICE_MASK");
    setenv("ACPP_VISIBILITY_MASK", visibility_mask != nullptr ? visibility_mask : "cuda", 1);
    setenv("PGACCEL_TEST_NO_DEVICE", "1", 1);
    execl(executable, executable, static_cast<char*>(nullptr));
    std::fprintf(stderr, "FAIL exec no-device H3 matrix: errno=%d\n", errno);
    _exit(127);
  }

  int status = 0;
  pid_t waited;
  do {
    waited = waitpid(child, &status, 0);
  } while (waited < 0 && errno == EINTR);
  if (waited != child || !WIFEXITED(status) || WEXITSTATUS(status) != 0) {
    std::fprintf(stderr, "FAIL no-device H3 matrix child: status=%d errno=%d\n", status, errno);
    return false;
  }
  return true;
}

// ---------------------------------------------------------------------------
static const char* selected_test(int argc, char** argv) {
  const char* filter = std::getenv("PGACCEL_H3_TEST");
  for (int i = 1; i < argc; i++) {
    if (std::strcmp(argv[i], "--test") == 0 && i + 1 < argc)
      return argv[i + 1];
    if (std::strncmp(argv[i], "--test=", 7) == 0)
      return argv[i] + 7;
    if (std::strncmp(argv[i], "--only=", 7) == 0)
      return argv[i] + 7;
  }
  return filter;
}

static bool should_run_test(const char* selected, const char* name) {
  return selected == nullptr || selected[0] == '\0' || std::strcmp(selected, name) == 0;
}

// ---------------------------------------------------------------------------
// Main
// ---------------------------------------------------------------------------
int main(int argc, char** argv) {
  printf("=== pg_accel H3 kernel tests ===\n\n");

  if (std::getenv("PGACCEL_TEST_NO_DEVICE") != nullptr) {
    test_no_device_paths();
    printf("\n=== Results: %d passed, %d failed ===\n", g_pass, g_fail);
    return g_fail > 0 ? 1 : 0;
  }

  ASSERT_TRUE("no-device H3 child", argc > 0 && argv[0] != nullptr && run_no_device_child(argv[0]));

  const char* selected = selected_test(argc, argv);
  bool ran_any = false;
#define RUN_TEST(fn)                      \
  do {                                    \
    if (should_run_test(selected, #fn)) { \
      ran_any = true;                     \
      fn();                               \
    }                                     \
  } while (0)

  RUN_TEST(test_get_resolution);
  RUN_TEST(test_get_base_cell);
  RUN_TEST(test_is_valid_cell);
  RUN_TEST(test_is_pentagon);
  RUN_TEST(test_is_res_class_iii);
  RUN_TEST(test_cell_to_parent);
  RUN_TEST(test_cell_to_parent_resident);
  RUN_TEST(test_cell_to_center_child);
  RUN_TEST(test_grid_distance);
  RUN_TEST(test_lat_lng_to_cell);
  RUN_TEST(test_lat_lng_to_cell_bulk_edge_randomized);
  RUN_TEST(test_lat_lng_to_cell_fp32_exact_matrix);
  RUN_TEST(test_cell_to_parent_count_bulk);
  RUN_TEST(test_lat_lng_count_bulk);
  RUN_TEST(test_lat_lng_count_bulk_all_res_duplicate_edges);
  RUN_TEST(test_lat_lng_count_bulk_f32_exact_all_res_edge_randomized);
  RUN_TEST(test_lat_lng_count_resident_low_high_matrix);
  RUN_TEST(test_lat_lng_res7_exact_edge_fixups);
  RUN_TEST(test_lat_lng_to_cell_fp64_bulk);
  RUN_TEST(test_null_pointers);
  RUN_TEST(test_api_contract_matrix);

  // Exact variable-output kernel plus topology fail-closed contracts.
  RUN_TEST(test_grid_disk);
  RUN_TEST(test_grid_ring_unsafe);
  RUN_TEST(test_cell_to_children);
  RUN_TEST(test_cell_to_boundary);
  RUN_TEST(test_polyfill);
  RUN_TEST(test_cells_to_multi_polygon);
  RUN_TEST(test_executed_h3_branch_matrix);

#undef RUN_TEST

  if (!ran_any) {
    fprintf(stderr, "FAIL: no H3 test matched '%s'\n", selected ? selected : "");
    g_fail++;
  }

  ASSERT_STATUS_OK("pgaccel_shutdown", pgaccel_shutdown());
  printf("\n=== Results: %d passed, %d failed ===\n", g_pass, g_fail);
  return g_fail > 0 ? 1 : 0;
}

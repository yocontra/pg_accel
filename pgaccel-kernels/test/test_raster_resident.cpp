#include <cmath>
#include <cstdint>
#include <cstdio>
#include <cstring>
#include <functional>
#include <limits>
#include <stdexcept>
#include <string>
#include <utility>
#include <vector>

#include "pgaccel_expr.h"
#include "pgaccel_ffi.h"

namespace {

int g_passed = 0;
int g_failed = 0;

void require(bool condition, const std::string& message) {
  if (!condition)
    throw std::runtime_error(message);
}

template <typename Fn>
void run_test(const char* name, Fn&& function) {
  try {
    function();
    ++g_passed;
    std::printf("  PASS: %s\n", name);
  } catch (const std::exception& error) {
    ++g_failed;
    std::fprintf(stderr, "  FAIL: %s: %s\n", name, error.what());
  }
}

template <typename T>
class DeviceBuffer {
 public:
  DeviceBuffer() = default;
  explicit DeviceBuffer(const std::vector<T>& values) { upload(values); }
  DeviceBuffer(const DeviceBuffer&) = delete;
  DeviceBuffer& operator=(const DeviceBuffer&) = delete;
  ~DeviceBuffer() {
    if (pointer_ != nullptr)
      pgaccel_expr_device_free(pointer_);
  }

  void upload(const std::vector<T>& values) {
    require(pointer_ == nullptr, "device buffer uploaded twice");
    count_ = values.size();
    if (values.empty())
      return;
    void* raw = nullptr;
    const size_t bytes = values.size() * sizeof(T);
    require(pgaccel_expr_device_alloc_copy(values.data(), bytes, &raw) == PGACCEL_OK,
            "device upload failed");
    require(raw != nullptr, "device upload returned null");
    pointer_ = raw;
  }

  [[nodiscard]] T* pointer() const { return static_cast<T*>(pointer_); }
  [[nodiscard]] size_t count() const { return count_; }
  [[nodiscard]] size_t bytes() const { return count_ * sizeof(T); }

  [[nodiscard]] std::vector<T> download() const {
    std::vector<T> values(count_);
    if (count_ != 0) {
      require(pgaccel_expr_device_copy_to_host(values.data(), pointer_, bytes()) == PGACCEL_OK,
              "device download failed");
    }
    return values;
  }

 private:
  void* pointer_ = nullptr;
  size_t count_ = 0;
};

size_t pixel_width(uint32_t tag) {
  switch (tag) {
    case PGACCEL_RESIDENT_RASTER_BOOL:
    case PGACCEL_RESIDENT_RASTER_UINT2:
    case PGACCEL_RESIDENT_RASTER_UINT4:
    case PGACCEL_RESIDENT_RASTER_INT8:
    case PGACCEL_RESIDENT_RASTER_UINT8:
      return 1;
    case PGACCEL_RESIDENT_RASTER_INT16:
    case PGACCEL_RESIDENT_RASTER_UINT16:
      return 2;
    case PGACCEL_RESIDENT_RASTER_INT32:
    case PGACCEL_RESIDENT_RASTER_UINT32:
    case PGACCEL_RESIDENT_RASTER_FLOAT32:
      return 4;
    case PGACCEL_RESIDENT_RASTER_FLOAT64:
      return 8;
    default:
      return 0;
  }
}

void append_integer_le(std::vector<uint8_t>* bytes, uint64_t value, size_t width) {
  for (size_t byte = 0; byte < width; ++byte)
    bytes->push_back(static_cast<uint8_t>((value >> (byte * 8)) & 0xffu));
}

void append_f32_le(std::vector<uint8_t>* bytes, float value) {
  uint32_t bits = 0;
  std::memcpy(&bits, &value, sizeof(bits));
  append_integer_le(bytes, bits, sizeof(bits));
}

void append_f64_le(std::vector<uint8_t>* bytes, double value) {
  uint64_t bits = 0;
  std::memcpy(&bits, &value, sizeof(bits));
  append_integer_le(bytes, bits, sizeof(bits));
}

pgaccel_resident_raster_row row(uint32_t width, uint32_t height, uint32_t first_band,
                                uint32_t band_count) {
  pgaccel_resident_raster_row value{};
  value.width = width;
  value.height = height;
  value.first_band = first_band;
  value.band_count = band_count;
  value.srid = 4326;
  value.scale_x = 1.0;
  value.scale_y = -1.0;
  return value;
}

pgaccel_resident_raster_band band(uint32_t pixel_type, uint32_t flags = 0, double nodata = 0.0) {
  return {pixel_type, flags, nodata};
}

struct CaseData {
  std::vector<uint8_t> pixels;
  std::vector<uint64_t> band_offsets;
  std::vector<pgaccel_resident_raster_row> rows;
  std::vector<pgaccel_resident_raster_band> bands;
  std::vector<uint8_t> nulls;
  std::vector<pgaccel_resident_raster_reclass_rule> rules;
  std::vector<uint64_t> output_offsets;
  std::vector<uint8_t> output_pixels;
  std::vector<uint8_t> row_actions;
  uint32_t output_pixel_type = PGACCEL_RESIDENT_RASTER_UINT8;
  size_t max_total_pixels = 0;
  size_t max_chunk_pixels = 1;
};

CaseData single_band_case(uint32_t input_type, const std::vector<uint8_t>& pixels,
                          size_t pixel_count,
                          std::vector<pgaccel_resident_raster_reclass_rule> rules,
                          uint32_t output_type = PGACCEL_RESIDENT_RASTER_UINT8) {
  require(pixel_count <= UINT32_MAX, "test pixel count exceeds raster width");
  require(pixels.size() == pixel_count * pixel_width(input_type),
          "test input lane has wrong byte count");
  CaseData data;
  data.pixels = pixels;
  data.band_offsets = {0, pixels.size()};
  data.rows = {row(static_cast<uint32_t>(pixel_count), 1, 0, 1)};
  data.bands = {band(input_type)};
  data.rules = std::move(rules);
  data.output_pixel_type = output_type;
  data.output_offsets = {0, pixel_count * pixel_width(output_type)};
  data.output_pixels.assign(data.output_offsets.back(), 0xa5);
  data.row_actions = {0xa5};
  data.max_total_pixels = pixel_count;
  data.max_chunk_pixels = std::max<size_t>(1, pixel_count);
  return data;
}

class ResidentCase {
 public:
  explicit ResidentCase(const CaseData& data)
      : host_(data), pixels_(data.pixels), band_offsets_(data.band_offsets), rows_(data.rows),
        bands_(data.bands), nulls_(data.nulls), rules_(data.rules),
        output_offsets_(data.output_offsets), output_pixels_(data.output_pixels),
        row_actions_(data.row_actions),
        validation_(std::vector<pgaccel_resident_raster_validation_scratch>(1)) {
    request.abi_version = PGACCEL_RESIDENT_RASTER_ABI_VERSION;
    request.input.abi_version = PGACCEL_RESIDENT_RASTER_ABI_VERSION;
    request.input.pixels = pixels_.pointer();
    request.input.pixels_bytes = pixels_.bytes();
    request.input.band_offsets = band_offsets_.pointer();
    request.input.band_offsets_bytes = band_offsets_.bytes();
    request.input.rows = rows_.pointer();
    request.input.rows_bytes = rows_.bytes();
    request.input.bands = bands_.pointer();
    request.input.bands_bytes = bands_.bytes();
    request.input.nulls = nulls_.pointer();
    request.input.nulls_bytes = nulls_.bytes();
    request.input.row_count = rows_.count();
    request.input.band_count = bands_.count();
    request.count = rows_.count();
    request.output_pixel_type = data.output_pixel_type;
    request.rules = rules_.pointer();
    request.rules_bytes = rules_.bytes();
    request.rule_count = rules_.count();
    request.output_offsets = output_offsets_.pointer();
    request.output_offsets_bytes = output_offsets_.bytes();
    request.output_pixels = output_pixels_.pointer();
    request.output_pixels_bytes = output_pixels_.bytes();
    request.row_actions = row_actions_.pointer();
    request.row_actions_bytes = row_actions_.bytes();
    request.validation_scratch = validation_.pointer();
    request.validation_scratch_bytes = validation_.bytes();
    request.max_total_pixels = data.max_total_pixels;
    request.max_chunk_pixels = data.max_chunk_pixels;
  }

  [[nodiscard]] pgaccel_status invoke(int32_t* detail) {
    return pgaccel_raster_reclass_resident_ex(&request, detail);
  }
  [[nodiscard]] std::vector<uint8_t> output() const { return output_pixels_.download(); }
  [[nodiscard]] std::vector<uint8_t> actions() const { return row_actions_.download(); }
  [[nodiscard]] std::vector<uint8_t> input_pixels() const { return pixels_.download(); }
  [[nodiscard]] std::vector<pgaccel_resident_raster_row> input_rows() const {
    return rows_.download();
  }
  [[nodiscard]] std::vector<pgaccel_resident_raster_band> input_bands() const {
    return bands_.download();
  }
  [[nodiscard]] pgaccel_resident_raster_validation_scratch validation() const {
    return validation_.download().at(0);
  }
  [[nodiscard]] const CaseData& host() const { return host_; }

  pgaccel_raster_reclass_resident_request request{};

 private:
  CaseData host_;
  DeviceBuffer<uint8_t> pixels_;
  DeviceBuffer<uint64_t> band_offsets_;
  DeviceBuffer<pgaccel_resident_raster_row> rows_;
  DeviceBuffer<pgaccel_resident_raster_band> bands_;
  DeviceBuffer<uint8_t> nulls_;
  DeviceBuffer<pgaccel_resident_raster_reclass_rule> rules_;
  DeviceBuffer<uint64_t> output_offsets_;
  DeviceBuffer<uint8_t> output_pixels_;
  DeviceBuffer<uint8_t> row_actions_;
  DeviceBuffer<pgaccel_resident_raster_validation_scratch> validation_;
};

int32_t mapped_detail(const pgaccel_resident_raster_validation_scratch& validation) {
  if ((validation.failures & PGACCEL_RASTER_VALIDATION_NUMERIC_OVERFLOW) != 0)
    return PGACCEL_RASTER_DETAIL_NUMERIC_OVERFLOW;
  if ((validation.failures & PGACCEL_RASTER_VALIDATION_RULES) != 0)
    return PGACCEL_RASTER_DETAIL_RULES;
  if ((validation.failures & PGACCEL_RASTER_VALIDATION_VIEW) != 0)
    return PGACCEL_RASTER_DETAIL_VIEW;
  if ((validation.failures & PGACCEL_RASTER_VALIDATION_OFFSETS) != 0)
    return PGACCEL_RASTER_DETAIL_OFFSETS;
  if ((validation.failures & PGACCEL_RASTER_VALIDATION_CAPACITY) != 0)
    return PGACCEL_RASTER_DETAIL_CAPACITY;
  if ((validation.failures & PGACCEL_RASTER_VALIDATION_BYTE_BUDGET) != 0)
    return PGACCEL_RASTER_DETAIL_BYTE_BUDGET;
  return PGACCEL_RASTER_DETAIL_NONE;
}

void expect_valid(ResidentCase* test_case, const std::vector<uint8_t>& output,
                  const std::vector<uint8_t>& actions) {
  int32_t launch_detail = -1;
  require(test_case->invoke(&launch_detail) == PGACCEL_OK, "valid launch failed");
  require(launch_detail == PGACCEL_RASTER_DETAIL_NONE, "valid launch detail is nonzero");
  const auto validation = test_case->validation();
  require(mapped_detail(validation) == PGACCEL_RASTER_DETAIL_NONE,
          "valid device validation failed");
  const auto actual_output = test_case->output();
  if (actual_output != output) {
    size_t first_difference = 0;
    while (first_difference < actual_output.size() && first_difference < output.size() &&
           actual_output[first_difference] == output[first_difference])
      ++first_difference;
    char message[192];
    std::snprintf(message, sizeof(message),
                  "valid output bytes differ at %zu (actual=%u expected=%u; sizes=%zu/%zu)",
                  first_difference,
                  first_difference < actual_output.size() ? actual_output[first_difference] : 0,
                  first_difference < output.size() ? output[first_difference] : 0,
                  actual_output.size(), output.size());
    throw std::runtime_error(message);
  }
  require(test_case->actions() == actions, "valid row actions differ");
}

void expect_device_failure(ResidentCase* test_case, int32_t expected_detail) {
  const auto output_before = test_case->output();
  const auto actions_before = test_case->actions();
  int32_t launch_detail = -1;
  require(test_case->invoke(&launch_detail) == PGACCEL_OK,
          "device validation failure changed launch status");
  require(launch_detail == PGACCEL_RASTER_DETAIL_NONE,
          "device validation failure changed launch detail");
  require(mapped_detail(test_case->validation()) == expected_detail,
          "post-borrow device detail differs");
  require(test_case->output() == output_before, "failed device validation wrote output pixels");
  require(test_case->actions() == actions_before, "failed device validation wrote row actions");
}

void expect_host_failure(ResidentCase* test_case, int32_t expected_detail) {
  const auto output_before = test_case->output();
  const auto actions_before = test_case->actions();
  const uint64_t executions_before = pgaccel_gpu_exec_count();
  int32_t detail = -1;
  require(test_case->invoke(&detail) == PGACCEL_INVALID_ARGUMENT,
          "host contract failure did not return INVALID_ARGUMENT");
  require(detail == expected_detail, "host contract detail differs");
  require(pgaccel_gpu_exec_count() == executions_before,
          "host contract failure recorded a GPU execution");
  require(test_case->output() == output_before, "host contract failure wrote output pixels");
  require(test_case->actions() == actions_before, "host contract failure wrote row actions");
}

void test_abi_layout() {
  static_assert(sizeof(pgaccel_resident_raster_row) == 72);
  static_assert(sizeof(pgaccel_resident_raster_band) == 16);
  static_assert(sizeof(pgaccel_resident_raster_view) == 104);
  static_assert(sizeof(pgaccel_resident_raster_reclass_rule) == 16);
  static_assert(sizeof(pgaccel_resident_raster_validation_scratch) == 24);
  static_assert(sizeof(pgaccel_raster_reclass_resident_request) == 240);
  require(PGACCEL_RASTER_VALIDATION_VIEW == 1u, "view failure bit drifted");
  require(PGACCEL_RASTER_VALIDATION_RULES == 2u, "rules failure bit drifted");
  require(PGACCEL_RASTER_VALIDATION_OFFSETS == 4u, "offset failure bit drifted");
  require(PGACCEL_RASTER_VALIDATION_CAPACITY == 8u, "capacity failure bit drifted");
  require(PGACCEL_RASTER_VALIDATION_BYTE_BUDGET == 16u, "budget failure bit drifted");
  require(PGACCEL_RASTER_VALIDATION_NUMERIC_OVERFLOW == 32u, "numeric failure bit drifted");
  require(PGACCEL_RESIDENT_RASTER_ROWS_PER_VALIDATION_LAUNCH == 65'536u, "row launch size drifted");
  require(PGACCEL_RESIDENT_RASTER_MAX_LAUNCH_CHUNKS == 4096u, "launch chunk cap drifted");
}

void test_row_actions_and_multiband_preservation() {
  CaseData data;
  data.pixels = {1, 2};
  data.band_offsets = {0, 0, 2};
  data.rows = {pgaccel_resident_raster_row{}, row(2, 2, 0, 0), row(0, 3, 0, 1), row(2, 1, 1, 1)};
  data.bands = {band(PGACCEL_RESIDENT_RASTER_UINT8), band(PGACCEL_RESIDENT_RASTER_UINT8)};
  data.nulls = {1, 0, 0, 0};
  data.rules = {{1, 7}, {2, 8}};
  data.output_offsets = {0, 0, 0, 0, 2};
  data.output_pixels = {0xa5, 0xa5};
  data.row_actions = {0xa5, 0xa5, 0xa5, 0xa5};
  data.max_total_pixels = 2;
  data.max_chunk_pixels = 1;
  ResidentCase actions(data);
  expect_valid(&actions, {7, 8},
               {PGACCEL_RASTER_ROW_NULL, PGACCEL_RASTER_ROW_PASSTHROUGH,
                PGACCEL_RASTER_ROW_RECLASSIFIED, PGACCEL_RASTER_ROW_RECLASSIFIED});

  CaseData multiband;
  multiband.pixels = {1, 2, 0xaa, 0xbb, 0xcc, 0xdd};
  multiband.band_offsets = {0, 2, 6};
  multiband.rows = {row(2, 1, 0, 2)};
  multiband.bands = {
      band(PGACCEL_RESIDENT_RASTER_UINT8),
      band(PGACCEL_RESIDENT_RASTER_INT16, PGACCEL_RESIDENT_RASTER_BAND_HAS_NODATA, -123.0)};
  multiband.rules = {{1, 7}, {2, 8}};
  multiband.output_offsets = {0, 2};
  multiband.output_pixels = {0xa5, 0xa5};
  multiband.row_actions = {0xa5};
  multiband.max_total_pixels = 2;
  multiband.max_chunk_pixels = 1;
  ResidentCase untouched(multiband);
  const auto rows_before = untouched.input_rows();
  const auto bands_before = untouched.input_bands();
  expect_valid(&untouched, {7, 8}, {PGACCEL_RASTER_ROW_RECLASSIFIED});
  require(untouched.input_pixels() == multiband.pixels, "multiband input pixels changed");
  require(std::memcmp(untouched.input_rows().data(), rows_before.data(),
                      rows_before.size() * sizeof(rows_before[0])) == 0,
          "multiband row metadata changed");
  require(std::memcmp(untouched.input_bands().data(), bands_before.data(),
                      bands_before.size() * sizeof(bands_before[0])) == 0,
          "multiband band metadata changed");
}

void test_integer_source_and_output_matrix() {
  struct SourceCase {
    uint32_t tag;
    uint64_t low_bits;
    uint64_t high_bits;
    int64_t low;
    int64_t high;
  };
  const SourceCase sources[] = {
      {PGACCEL_RESIDENT_RASTER_BOOL, 0, 1, 0, 1},
      {PGACCEL_RESIDENT_RASTER_UINT2, 0, 3, 0, 3},
      {PGACCEL_RESIDENT_RASTER_UINT4, 0, 15, 0, 15},
      {PGACCEL_RESIDENT_RASTER_INT8, 0x80, 0x7f, INT8_MIN, INT8_MAX},
      {PGACCEL_RESIDENT_RASTER_UINT8, 0, UINT8_MAX, 0, UINT8_MAX},
      {PGACCEL_RESIDENT_RASTER_INT16, 0x8000, 0x7fff, INT16_MIN, INT16_MAX},
      {PGACCEL_RESIDENT_RASTER_UINT16, 0, UINT16_MAX, 0, UINT16_MAX},
      {PGACCEL_RESIDENT_RASTER_INT32, 0x80000000u, 0x7fffffffu, INT32_MIN, INT32_MAX},
      {PGACCEL_RESIDENT_RASTER_UINT32, 0, UINT32_MAX, 0, static_cast<int64_t>(UINT32_MAX)},
  };
  for (const SourceCase& source : sources) {
    std::vector<uint8_t> pixels;
    append_integer_le(&pixels, source.low_bits, pixel_width(source.tag));
    append_integer_le(&pixels, source.high_bits, pixel_width(source.tag));
    CaseData data = single_band_case(source.tag, pixels, 2, {{source.low, 11}, {source.high, 22}});
    if (source.low > source.high)
      data.rules = {{source.high, 22}, {source.low, 11}};
    ResidentCase test_case(data);
    expect_valid(&test_case, {11, 22}, {PGACCEL_RASTER_ROW_RECLASSIFIED});
  }

  struct OutputCase {
    uint32_t tag;
    int64_t minimum;
    int64_t maximum;
  };
  const OutputCase outputs[] = {
      {PGACCEL_RESIDENT_RASTER_BOOL, 0, 1},
      {PGACCEL_RESIDENT_RASTER_UINT2, 0, 3},
      {PGACCEL_RESIDENT_RASTER_UINT4, 0, 15},
      {PGACCEL_RESIDENT_RASTER_INT8, INT8_MIN, INT8_MAX},
      {PGACCEL_RESIDENT_RASTER_UINT8, 0, UINT8_MAX},
      {PGACCEL_RESIDENT_RASTER_INT16, INT16_MIN, INT16_MAX},
      {PGACCEL_RESIDENT_RASTER_UINT16, 0, UINT16_MAX},
      {PGACCEL_RESIDENT_RASTER_INT32, INT32_MIN, INT32_MAX},
      {PGACCEL_RESIDENT_RASTER_UINT32, 0, static_cast<int64_t>(UINT32_MAX)},
  };
  std::vector<uint8_t> input;
  append_integer_le(&input, 1, 4);
  append_integer_le(&input, 2, 4);
  for (const OutputCase& output : outputs) {
    CaseData data = single_band_case(PGACCEL_RESIDENT_RASTER_INT32, input, 2,
                                     {{1, output.minimum}, {2, output.maximum}}, output.tag);
    std::vector<uint8_t> expected;
    append_integer_le(&expected, static_cast<uint64_t>(output.minimum), pixel_width(output.tag));
    append_integer_le(&expected, static_cast<uint64_t>(output.maximum), pixel_width(output.tag));
    ResidentCase test_case(data);
    expect_valid(&test_case, expected, {PGACCEL_RASTER_ROW_RECLASSIFIED});
  }
}

void test_float_epsilon_nan_and_infinity() {
  constexpr float epsilon_f = 1.1920928955078125e-7f;
  std::vector<uint8_t> f32;
  for (float value :
       {0.0f, epsilon_f, std::nextafter(epsilon_f, INFINITY),
        std::numeric_limits<float>::quiet_NaN(), std::numeric_limits<float>::infinity(),
        -std::numeric_limits<float>::infinity(), 1.0f})
    append_f32_le(&f32, value);
  CaseData f32_data = single_band_case(PGACCEL_RESIDENT_RASTER_FLOAT32, f32, 7, {{0, 5}, {1, 7}});
  ResidentCase f32_case(f32_data);
  expect_valid(&f32_case, {5, 5, 0, 0, 0, 0, 7}, {PGACCEL_RASTER_ROW_RECLASSIFIED});

  constexpr double epsilon = 1.1920928955078125e-7;
  std::vector<uint8_t> f64;
  for (double value :
       {7.0, 7.0 + epsilon, std::nextafter(7.0 + epsilon, INFINITY),
        std::numeric_limits<double>::quiet_NaN(), std::numeric_limits<double>::infinity(),
        -std::numeric_limits<double>::infinity()})
    append_f64_le(&f64, value);
  CaseData f64_data = single_band_case(PGACCEL_RESIDENT_RASTER_FLOAT64, f64, 6, {{7, 9}});
  ResidentCase f64_case(f64_data);
  expect_valid(&f64_case, {9, 9, 0, 0, 0, 0}, {PGACCEL_RASTER_ROW_RECLASSIFIED});
}

void test_nodata_flags_are_ordinary_source_values() {
  std::vector<uint8_t> pixels;
  append_integer_le(&pixels, 1, 2);
  append_integer_le(&pixels, 2, 2);
  append_integer_le(&pixels, 3, 2);
  CaseData all = single_band_case(PGACCEL_RESIDENT_RASTER_INT16, pixels, 3, {{-7, 99}});
  all.bands[0] =
      band(PGACCEL_RESIDENT_RASTER_INT16,
           PGACCEL_RESIDENT_RASTER_BAND_HAS_NODATA | PGACCEL_RESIDENT_RASTER_BAND_IS_NODATA, -7.0);
  ResidentCase all_case(all);
  expect_valid(&all_case, {99, 99, 99}, {PGACCEL_RASTER_ROW_RECLASSIFIED});

  pixels.clear();
  append_integer_le(&pixels, static_cast<uint64_t>(-7), 2);
  append_integer_le(&pixels, 8, 2);
  CaseData ordinary = single_band_case(PGACCEL_RESIDENT_RASTER_INT16, pixels, 2, {{-7, 42}});
  ordinary.bands[0] =
      band(PGACCEL_RESIDENT_RASTER_INT16, PGACCEL_RESIDENT_RASTER_BAND_HAS_NODATA, -7.0);
  ResidentCase ordinary_case(ordinary);
  expect_valid(&ordinary_case, {42, 0}, {PGACCEL_RASTER_ROW_RECLASSIFIED});
}

void test_endian_normalized_source_lanes() {
  const std::vector<uint8_t> little_wkb_normalized = {0x34, 0x12};
  std::vector<uint8_t> big_wkb = {0x12, 0x34};
  std::vector<uint8_t> big_wkb_normalized = {big_wkb[1], big_wkb[0]};
  for (const auto& normalized : {little_wkb_normalized, big_wkb_normalized}) {
    CaseData data = single_band_case(PGACCEL_RESIDENT_RASTER_INT16, normalized, 1, {{0x1234, 77}});
    ResidentCase test_case(data);
    expect_valid(&test_case, {77}, {PGACCEL_RASTER_ROW_RECLASSIFIED});
  }
}

void test_slice_zero_output_and_rule_boundaries() {
  {
    CaseData data;
    data.pixels = {1, 2, 3, 4};
    data.band_offsets = {0, 2, 4};
    data.rows = {row(2, 1, 0, 1), row(2, 1, 1, 1)};
    data.bands = {band(PGACCEL_RESIDENT_RASTER_UINT8), band(PGACCEL_RESIDENT_RASTER_UINT8)};
    data.rules = {{3, 7}, {4, 8}};
    data.output_offsets = {0, 2, 4};
    data.output_pixels.assign(4, 0xa5);
    data.row_actions.assign(2, 0xa5);
    data.max_total_pixels = 2;
    data.max_chunk_pixels = 1;
    ResidentCase sliced(data);
    sliced.request.first_row = 1;
    sliced.request.count = 1;
    sliced.request.output_offsets += 1;
    sliced.request.output_offsets_bytes = 2 * sizeof(uint64_t);
    sliced.request.row_actions_bytes = 1;
    expect_valid(&sliced, {0xa5, 0xa5, 7, 8}, {PGACCEL_RASTER_ROW_RECLASSIFIED, 0xa5});
    const auto validation = sliced.validation();
    require(validation.first_output_offset == 2 && validation.last_output_offset == 4,
            "nonzero slice offsets were not preserved");
  }
  {
    CaseData data;
    data.band_offsets = {0};
    data.rows = {pgaccel_resident_raster_row{}, pgaccel_resident_raster_row{}};
    data.nulls = {1, 1};
    data.rules = {{0, 1}};
    data.output_offsets = {0, 0, 0};
    data.row_actions.assign(2, 0xa5);
    data.max_chunk_pixels = 1;
    ResidentCase all_null(data);
    expect_valid(&all_null, {}, {PGACCEL_RASTER_ROW_NULL, PGACCEL_RASTER_ROW_NULL});
  }
  {
    CaseData data;
    data.band_offsets = {0, 0, 0};
    data.rows = {row(0, 3, 0, 1), row(4, 0, 1, 1)};
    data.bands = {band(PGACCEL_RESIDENT_RASTER_UINT8), band(PGACCEL_RESIDENT_RASTER_UINT8)};
    data.rules = {{0, 1}};
    data.output_offsets = {0, 0, 0};
    data.row_actions.assign(2, 0xa5);
    data.max_chunk_pixels = 1;
    ResidentCase zero_pixels(data);
    expect_valid(&zero_pixels, {},
                 {PGACCEL_RASTER_ROW_RECLASSIFIED, PGACCEL_RASTER_ROW_RECLASSIFIED});
  }
  {
    std::vector<pgaccel_resident_raster_reclass_rule> rules;
    for (int64_t source = 0; source < 64; ++source)
      rules.push_back({source, 1});
    ResidentCase max_rules(single_band_case(PGACCEL_RESIDENT_RASTER_UINT8, {63}, 1, rules));
    expect_valid(&max_rules, {1}, {PGACCEL_RASTER_ROW_RECLASSIFIED});
    rules.push_back({64, 1});
    ResidentCase too_many(single_band_case(PGACCEL_RESIDENT_RASTER_UINT8, {63}, 1, rules));
    expect_host_failure(&too_many, PGACCEL_RASTER_DETAIL_CONTRACT);
  }
  {
    ResidentCase unmatched(single_band_case(PGACCEL_RESIDENT_RASTER_UINT8, {1, 9}, 2, {{1, 7}}));
    expect_valid(&unmatched, {7, 0}, {PGACCEL_RASTER_ROW_RECLASSIFIED});
  }
  {
    CaseData data =
        single_band_case(PGACCEL_RESIDENT_RASTER_UINT8, {1, 2, 3}, 3, {{1, 7}, {2, 8}, {3, 9}});
    data.max_chunk_pixels = 2;
    ResidentCase final_partial_chunk(data);
    expect_valid(&final_partial_chunk, {7, 8, 9}, {PGACCEL_RASTER_ROW_RECLASSIFIED});
  }
}

void test_signed_zero_and_nan_nodata() {
  std::vector<uint8_t> raw;
  append_f64_le(&raw, 123.0);
  {
    CaseData data = single_band_case(PGACCEL_RESIDENT_RASTER_FLOAT64, raw, 1, {{0, 5}});
    data.bands[0] = band(
        PGACCEL_RESIDENT_RASTER_FLOAT64,
        PGACCEL_RESIDENT_RASTER_BAND_HAS_NODATA | PGACCEL_RESIDENT_RASTER_BAND_IS_NODATA, -0.0);
    ResidentCase signed_zero(data);
    expect_valid(&signed_zero, {5}, {PGACCEL_RASTER_ROW_RECLASSIFIED});
  }
  {
    CaseData data = single_band_case(PGACCEL_RESIDENT_RASTER_FLOAT64, raw, 1, {{0, 5}});
    data.bands[0] =
        band(PGACCEL_RESIDENT_RASTER_FLOAT64,
             PGACCEL_RESIDENT_RASTER_BAND_HAS_NODATA | PGACCEL_RESIDENT_RASTER_BAND_IS_NODATA,
             std::numeric_limits<double>::quiet_NaN());
    ResidentCase nan_nodata(data);
    expect_valid(&nan_nodata, {0}, {PGACCEL_RASTER_ROW_RECLASSIFIED});
  }
  {
    CaseData data = single_band_case(PGACCEL_RESIDENT_RASTER_FLOAT64, raw, 1, {{0, 5}});
    data.bands[0].nodata = -0.0;
    ResidentCase noncanonical(data);
    expect_device_failure(&noncanonical, PGACCEL_RASTER_DETAIL_VIEW);
  }
}

void test_device_validation_failures_do_not_write() {
  const struct {
    uint32_t tag;
    uint8_t malformed;
  } low_bits[] = {{PGACCEL_RESIDENT_RASTER_BOOL, 0x02},
                  {PGACCEL_RESIDENT_RASTER_UINT2, 0x04},
                  {PGACCEL_RESIDENT_RASTER_UINT4, 0x10}};
  for (const auto& test : low_bits) {
    ResidentCase malformed(single_band_case(test.tag, {test.malformed}, 1, {{0, 1}}));
    expect_device_failure(&malformed, PGACCEL_RASTER_DETAIL_VIEW);
  }

  {
    ResidentCase invalid(single_band_case(PGACCEL_RESIDENT_RASTER_UINT8, {1}, 1, {{2, 1}, {1, 2}}));
    expect_device_failure(&invalid, PGACCEL_RASTER_DETAIL_RULES);
  }
  {
    ResidentCase invalid(single_band_case(PGACCEL_RESIDENT_RASTER_UINT8, {1}, 1, {{1, 1}, {1, 2}}));
    expect_device_failure(&invalid, PGACCEL_RASTER_DETAIL_RULES);
  }
  {
    ResidentCase invalid(single_band_case(PGACCEL_RESIDENT_RASTER_UINT8, {1}, 1,
                                          {{static_cast<int64_t>(UINT32_MAX) + 1, 1}}));
    expect_device_failure(&invalid, PGACCEL_RASTER_DETAIL_RULES);
  }
  {
    ResidentCase invalid(single_band_case(PGACCEL_RESIDENT_RASTER_UINT8, {1}, 1, {{1, 256}}));
    expect_device_failure(&invalid, PGACCEL_RASTER_DETAIL_RULES);
  }
  {
    CaseData data = single_band_case(PGACCEL_RESIDENT_RASTER_UINT8, {1}, 1, {{1, 2}});
    data.nulls = {2};
    ResidentCase invalid(data);
    expect_device_failure(&invalid, PGACCEL_RASTER_DETAIL_VIEW);
  }
  {
    CaseData data = single_band_case(PGACCEL_RESIDENT_RASTER_UINT8, {1}, 1, {{1, 2}});
    data.bands[0].pixel_type = 9;
    ResidentCase invalid(data);
    expect_device_failure(&invalid, PGACCEL_RASTER_DETAIL_VIEW);
  }
  {
    CaseData data = single_band_case(PGACCEL_RESIDENT_RASTER_UINT8, {1}, 1, {{1, 2}});
    data.bands[0].flags = PGACCEL_RESIDENT_RASTER_BAND_IS_NODATA;
    ResidentCase invalid(data);
    expect_device_failure(&invalid, PGACCEL_RASTER_DETAIL_VIEW);
  }
  {
    CaseData data = single_band_case(PGACCEL_RESIDENT_RASTER_UINT8, {1}, 1, {{1, 2}});
    data.rows[0].scale_x = std::numeric_limits<double>::infinity();
    ResidentCase invalid(data);
    expect_device_failure(&invalid, PGACCEL_RASTER_DETAIL_VIEW);
  }
  {
    CaseData data = single_band_case(PGACCEL_RESIDENT_RASTER_UINT8, {1}, 1, {{1, 2}});
    data.band_offsets = {1, 2};
    ResidentCase invalid(data);
    expect_device_failure(&invalid, PGACCEL_RASTER_DETAIL_VIEW);
  }
  {
    CaseData data = single_band_case(PGACCEL_RESIDENT_RASTER_UINT8, {1}, 1, {{1, 2}});
    data.output_offsets = {0, 0};
    ResidentCase invalid(data);
    expect_device_failure(&invalid, PGACCEL_RASTER_DETAIL_OFFSETS);
  }
  {
    CaseData data = single_band_case(PGACCEL_RESIDENT_RASTER_UINT8, {1}, 1, {{1, 2}});
    data.output_offsets = {1, 2};
    ResidentCase invalid(data);
    expect_device_failure(&invalid, PGACCEL_RASTER_DETAIL_CAPACITY);
  }
  {
    CaseData data = single_band_case(PGACCEL_RESIDENT_RASTER_UINT8, {1, 2}, 2, {{1, 7}, {2, 8}});
    data.max_total_pixels = 1;
    data.max_chunk_pixels = 1;
    ResidentCase invalid(data);
    expect_device_failure(&invalid, PGACCEL_RASTER_DETAIL_BYTE_BUDGET);
  }
  {
    CaseData data =
        single_band_case(PGACCEL_RESIDENT_RASTER_FLOAT64, std::vector<uint8_t>(8, 0), 1, {{0, 1}});
    data.rows[0].width = UINT32_MAX;
    data.rows[0].height = UINT32_MAX;
    data.output_offsets = {0, 0};
    data.output_pixels.clear();
    data.max_total_pixels = 0;
    data.max_chunk_pixels = 1;
    ResidentCase invalid(data);
    expect_device_failure(&invalid, PGACCEL_RASTER_DETAIL_NUMERIC_OVERFLOW);
  }
  {
    CaseData data = single_band_case(PGACCEL_RESIDENT_RASTER_UINT8, {0}, 1, {{0, 1}},
                                     PGACCEL_RESIDENT_RASTER_INT32);
    data.rows[0].width = UINT32_MAX;
    data.rows[0].height = UINT32_MAX;
    data.output_offsets = {0, 0};
    data.output_pixels.clear();
    data.max_total_pixels = 0;
    data.max_chunk_pixels = 1;
    ResidentCase invalid(data);
    expect_device_failure(&invalid, PGACCEL_RASTER_DETAIL_NUMERIC_OVERFLOW);
  }
}

void test_host_contract_failures_do_not_write() {
  auto base = [] {
    return single_band_case(PGACCEL_RESIDENT_RASTER_UINT8, {1, 2}, 2, {{1, 7}, {2, 8}});
  };
  {
    ResidentCase invalid(base());
    invalid.request.input.rows_bytes--;
    expect_host_failure(&invalid, PGACCEL_RASTER_DETAIL_CONTRACT);
  }
  {
    ResidentCase invalid(base());
    invalid.request.input.bands_bytes--;
    expect_host_failure(&invalid, PGACCEL_RASTER_DETAIL_CONTRACT);
  }
  {
    ResidentCase invalid(base());
    invalid.request.input.band_offsets_bytes--;
    expect_host_failure(&invalid, PGACCEL_RASTER_DETAIL_CONTRACT);
  }
  {
    ResidentCase invalid(base());
    invalid.request.output_offsets_bytes--;
    expect_host_failure(&invalid, PGACCEL_RASTER_DETAIL_CONTRACT);
  }
  {
    ResidentCase invalid(base());
    invalid.request.row_actions_bytes--;
    expect_host_failure(&invalid, PGACCEL_RASTER_DETAIL_CONTRACT);
  }
  {
    ResidentCase invalid(base());
    invalid.request.validation_scratch_bytes--;
    expect_host_failure(&invalid, PGACCEL_RASTER_DETAIL_CONTRACT);
  }
  {
    CaseData data = base();
    ResidentCase invalid(data);
    invalid.request.rules = data.rules.data();
    expect_host_failure(&invalid, PGACCEL_RASTER_DETAIL_CONTRACT);
  }
  {
    ResidentCase invalid(base());
    invalid.request.output_pixels = const_cast<uint8_t*>(invalid.request.input.pixels);
    expect_host_failure(&invalid, PGACCEL_RASTER_DETAIL_CONTRACT);
  }
  {
    ResidentCase invalid(base());
    invalid.request.output_pixels = const_cast<uint8_t*>(invalid.request.input.pixels) + 1;
    expect_host_failure(&invalid, PGACCEL_RASTER_DETAIL_CONTRACT);
  }
  {
    ResidentCase invalid(base());
    invalid.request.input.row_count = std::numeric_limits<size_t>::max();
    expect_host_failure(&invalid, PGACCEL_RASTER_DETAIL_NUMERIC_OVERFLOW);
  }
  {
    ResidentCase invalid(base());
    invalid.request.validation_scratch =
        reinterpret_cast<pgaccel_resident_raster_validation_scratch*>(
            reinterpret_cast<uint8_t*>(invalid.request.validation_scratch) + 1);
    expect_host_failure(&invalid, PGACCEL_RASTER_DETAIL_CONTRACT);
  }
  {
    ResidentCase invalid(base());
    invalid.request.validation_scratch =
        reinterpret_cast<pgaccel_resident_raster_validation_scratch*>(invalid.request.row_actions);
    expect_host_failure(&invalid, PGACCEL_RASTER_DETAIL_CONTRACT);
  }
  {
    ResidentCase invalid(base());
    invalid.request.output_pixels_bytes--;
    expect_host_failure(&invalid, PGACCEL_RASTER_DETAIL_CAPACITY);
  }
  {
    ResidentCase invalid(base());
    invalid.request.max_total_pixels = std::numeric_limits<size_t>::max();
    invalid.request.max_chunk_pixels = 1;
    expect_host_failure(&invalid, PGACCEL_RASTER_DETAIL_CAPACITY);
  }
  {
    ResidentCase invalid(base());
    invalid.request.output_pixel_type = PGACCEL_RESIDENT_RASTER_INT32;
    invalid.request.max_total_pixels = std::numeric_limits<size_t>::max();
    invalid.request.max_chunk_pixels = 1;
    expect_host_failure(&invalid, PGACCEL_RASTER_DETAIL_NUMERIC_OVERFLOW);
  }
  {
    ResidentCase invalid(base());
    invalid.request.max_chunk_pixels = 0;
    expect_host_failure(&invalid, PGACCEL_RASTER_DETAIL_CONTRACT);
  }
  {
    ResidentCase invalid(base());
    invalid.request.max_chunk_pixels = invalid.request.max_total_pixels + 1;
    expect_host_failure(&invalid, PGACCEL_RASTER_DETAIL_CONTRACT);
  }
  {
    CaseData data =
        single_band_case(PGACCEL_RESIDENT_RASTER_UINT8,
                         std::vector<uint8_t>(PGACCEL_RESIDENT_RASTER_MAX_LAUNCH_CHUNKS + 1, 0),
                         PGACCEL_RESIDENT_RASTER_MAX_LAUNCH_CHUNKS + 1, {{0, 1}});
    data.max_chunk_pixels = 1;
    ResidentCase invalid(data);
    expect_host_failure(&invalid, PGACCEL_RASTER_DETAIL_BYTE_BUDGET);
  }
  {
    CaseData data;
    data.band_offsets = {0};
    data.rows = {pgaccel_resident_raster_row{}};
    data.nulls = {1};
    data.rules = {{0, 1}};
    data.output_offsets = {0, 0};
    data.row_actions = {0xa5};
    data.max_chunk_pixels = 1;
    ResidentCase invalid(data);
    constexpr size_t hostile_count =
        static_cast<size_t>(PGACCEL_RESIDENT_RASTER_MAX_LAUNCH_CHUNKS) *
            PGACCEL_RESIDENT_RASTER_ROWS_PER_VALIDATION_LAUNCH +
        1;
    // Descriptor-only arithmetic regression: the row launch bound must fire
    // before inspecting spans that cannot be physically allocated in a test.
    invalid.request.input.row_count = hostile_count;
    invalid.request.input.rows_bytes = hostile_count * sizeof(pgaccel_resident_raster_row);
    invalid.request.input.nulls_bytes = hostile_count;
    invalid.request.count = hostile_count;
    invalid.request.output_offsets_bytes = (hostile_count + 1) * sizeof(uint64_t);
    invalid.request.row_actions_bytes = hostile_count;
    expect_host_failure(&invalid, PGACCEL_RASTER_DETAIL_BYTE_BUDGET);
  }
}

void test_chunk_boundary_stress() {
  constexpr size_t count = 65'537;
  CaseData data;
  data.pixels.resize(count);
  data.band_offsets.resize(count + 1);
  data.rows.reserve(count);
  data.bands.reserve(count);
  data.output_offsets.resize(count + 1);
  data.output_pixels.assign(count, 0xa5);
  data.row_actions.assign(count, 0xa5);
  for (size_t index = 0; index < count; ++index) {
    data.pixels[index] = static_cast<uint8_t>(index & 1);
    data.band_offsets[index] = index;
    data.output_offsets[index] = index;
    data.rows.push_back(row(1, 1, static_cast<uint32_t>(index), 1));
    data.bands.push_back(band(PGACCEL_RESIDENT_RASTER_UINT8));
  }
  data.band_offsets[count] = count;
  data.output_offsets[count] = count;
  data.rules = {{0, 7}, {1, 9}};
  data.max_total_pixels = count;
  data.max_chunk_pixels = 8192;

  ResidentCase stress(data);
  const uint64_t before = pgaccel_gpu_exec_count();
  int32_t detail = -1;
  require(stress.invoke(&detail) == PGACCEL_OK, "stress launch failed");
  require(detail == PGACCEL_RASTER_DETAIL_NONE, "stress launch detail differs");
  require(mapped_detail(stress.validation()) == PGACCEL_RASTER_DETAIL_NONE,
          "stress device validation failed");
  require(pgaccel_gpu_exec_count() == before + 1, "stress launch counter differs");
  const auto output = stress.output();
  const auto actions = stress.actions();
  for (size_t index = 0; index < count; ++index) {
    require(output[index] == ((index & 1) == 0 ? 7 : 9), "stress output differs");
    require(actions[index] == PGACCEL_RASTER_ROW_RECLASSIFIED, "stress action differs");
  }
}

}  // namespace

int main() {
  std::printf("=== pgaccel exact resident raster tests ===\n\n");
  if (pgaccel_init() != PGACCEL_OK) {
    std::fprintf(stderr, "pgaccel_init failed\n");
    return 1;
  }

  run_test("ABI layout and public failure bits", test_abi_layout);
  run_test("NULL/empty/zero-band actions and multiband preservation",
           test_row_actions_and_multiband_preservation);
  run_test("integer source and output extrema matrix", test_integer_source_and_output_matrix);
  run_test("float32/64 FLT_EQ epsilon, NaN, and infinity", test_float_epsilon_nan_and_infinity);
  run_test("HAS_NODATA/IS_NODATA source semantics", test_nodata_flags_are_ordinary_source_values);
  run_test("little/big WKB normalized source lanes", test_endian_normalized_source_lanes);
  run_test("nonzero slices, zero output, 64-rule cap, and unmatched zero",
           test_slice_zero_output_and_rule_boundaries);
  run_test("signed-zero and NaN nodata semantics", test_signed_zero_and_nan_nodata);
  run_test("device validation is typed and output-atomic",
           test_device_validation_failures_do_not_write);
  run_test("host spans/caps/overlap/overflow are hard failures",
           test_host_contract_failures_do_not_write);
  run_test(">chunk row and pixel stress", test_chunk_boundary_stress);

  require(pgaccel_shutdown() == PGACCEL_OK, "pgaccel_shutdown failed");
  std::printf("\n=== Results: %d passed, %d failed ===\n", g_passed, g_failed);
  return g_failed == 0 ? 0 : 1;
}

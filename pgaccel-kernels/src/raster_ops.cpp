#include <sycl/sycl.hpp>

#include <algorithm>
#include <cstdint>
#include <exception>
#include <limits>
#include <new>

#include "pgaccel_ffi.h"
#include "pgaccel_queue.h"

static sycl::queue& get_queue() {
  return pgaccel_require_queue();
}

/* ── Exact resident PostGIS Reclass ───────────────────────────── */

namespace {

struct RasterResidentSpan {
  uintptr_t begin;
  uintptr_t end;
  bool active;
};

bool raster_resident_exact_span(const void* pointer, size_t bytes, RasterResidentSpan* span) {
  if (bytes == 0) {
    *span = {0, 0, false};
    return pointer == nullptr;
  }
  if (pointer == nullptr)
    return false;
  const uintptr_t begin = reinterpret_cast<uintptr_t>(pointer);
  if (begin > std::numeric_limits<uintptr_t>::max() - bytes)
    return false;
  *span = {begin, begin + bytes, true};
  return true;
}

bool raster_resident_spans_overlap(const RasterResidentSpan& lhs, const RasterResidentSpan& rhs) {
  return lhs.active && rhs.active && lhs.begin < rhs.end && rhs.begin < lhs.end;
}

bool raster_resident_checked_bytes(size_t count, size_t width, size_t* bytes) {
  if (width == 0 || count > std::numeric_limits<size_t>::max() / width)
    return false;
  *bytes = count * width;
  return true;
}

bool raster_resident_launch_count_within_limit(size_t total, size_t chunk) {
  if (chunk == 0)
    return false;
  const size_t launches = total == 0 ? 0 : 1 + (total - 1) / chunk;
  return launches <= PGACCEL_RESIDENT_RASTER_MAX_LAUNCH_CHUNKS;
}

bool raster_resident_current_device_pointer(sycl::queue& queue, const void* pointer) {
  try {
    const sycl::usm::alloc allocation = sycl::get_pointer_type(pointer, queue.get_context());
    return (allocation == sycl::usm::alloc::device || allocation == sycl::usm::alloc::shared) &&
           sycl::get_pointer_device(pointer, queue.get_context()) == queue.get_device();
  } catch (...) {
    return false;
  }
}

size_t raster_resident_pixel_width(uint32_t pixel_type) {
  switch (pixel_type) {
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

inline uint32_t raster_resident_width_shift(size_t width) {
  return width == 1 ? 0 : (width == 2 ? 1 : (width == 4 ? 2 : 3));
}

bool raster_resident_integer_bounds(uint32_t pixel_type, int64_t* minimum, int64_t* maximum) {
  switch (pixel_type) {
    case PGACCEL_RESIDENT_RASTER_BOOL:
      *minimum = 0;
      *maximum = 1;
      return true;
    case PGACCEL_RESIDENT_RASTER_UINT2:
      *minimum = 0;
      *maximum = 3;
      return true;
    case PGACCEL_RESIDENT_RASTER_UINT4:
      *minimum = 0;
      *maximum = 15;
      return true;
    case PGACCEL_RESIDENT_RASTER_INT8:
      *minimum = INT8_MIN;
      *maximum = INT8_MAX;
      return true;
    case PGACCEL_RESIDENT_RASTER_UINT8:
      *minimum = 0;
      *maximum = UINT8_MAX;
      return true;
    case PGACCEL_RESIDENT_RASTER_INT16:
      *minimum = INT16_MIN;
      *maximum = INT16_MAX;
      return true;
    case PGACCEL_RESIDENT_RASTER_UINT16:
      *minimum = 0;
      *maximum = UINT16_MAX;
      return true;
    case PGACCEL_RESIDENT_RASTER_INT32:
      *minimum = INT32_MIN;
      *maximum = INT32_MAX;
      return true;
    case PGACCEL_RESIDENT_RASTER_UINT32:
      *minimum = 0;
      *maximum = static_cast<int64_t>(UINT32_MAX);
      return true;
    default:
      return false;
  }
}

inline uint16_t raster_resident_load_u16_le(const uint8_t* pointer) {
  return static_cast<uint16_t>(pointer[0]) |
         static_cast<uint16_t>(static_cast<uint16_t>(pointer[1]) << 8);
}

inline uint32_t raster_resident_load_u32_le(const uint8_t* pointer) {
  return static_cast<uint32_t>(pointer[0]) | (static_cast<uint32_t>(pointer[1]) << 8) |
         (static_cast<uint32_t>(pointer[2]) << 16) | (static_cast<uint32_t>(pointer[3]) << 24);
}

inline uint64_t raster_resident_load_u64_le(const uint8_t* pointer) {
  uint64_t value = 0;
  for (uint32_t byte = 0; byte < 8; ++byte)
    value |= static_cast<uint64_t>(pointer[byte]) << (byte * 8);
  return value;
}

inline double raster_resident_read_pixel(const uint8_t* pointer, uint32_t pixel_type) {
  switch (pixel_type) {
    case PGACCEL_RESIDENT_RASTER_BOOL:
    case PGACCEL_RESIDENT_RASTER_UINT2:
    case PGACCEL_RESIDENT_RASTER_UINT4:
    case PGACCEL_RESIDENT_RASTER_UINT8:
      return static_cast<double>(pointer[0]);
    case PGACCEL_RESIDENT_RASTER_INT8: {
      const int32_t value = pointer[0] < 0x80 ? static_cast<int32_t>(pointer[0])
                                              : static_cast<int32_t>(pointer[0]) - 0x100;
      return static_cast<double>(value);
    }
    case PGACCEL_RESIDENT_RASTER_INT16: {
      const uint16_t raw = raster_resident_load_u16_le(pointer);
      const int32_t value =
          raw < 0x8000 ? static_cast<int32_t>(raw) : static_cast<int32_t>(raw) - 0x10000;
      return static_cast<double>(value);
    }
    case PGACCEL_RESIDENT_RASTER_UINT16:
      return static_cast<double>(raster_resident_load_u16_le(pointer));
    case PGACCEL_RESIDENT_RASTER_INT32: {
      const uint32_t raw = raster_resident_load_u32_le(pointer);
      const int64_t value =
          raw < 0x80000000u ? static_cast<int64_t>(raw) : static_cast<int64_t>(raw) - 0x100000000ll;
      return static_cast<double>(value);
    }
    case PGACCEL_RESIDENT_RASTER_UINT32:
      return static_cast<double>(raster_resident_load_u32_le(pointer));
    case PGACCEL_RESIDENT_RASTER_FLOAT32:
      return static_cast<double>(sycl::bit_cast<float>(raster_resident_load_u32_le(pointer)));
    case PGACCEL_RESIDENT_RASTER_FLOAT64:
      return sycl::bit_cast<double>(raster_resident_load_u64_le(pointer));
    default:
      return 0.0;
  }
}

inline void raster_resident_write_integer(uint8_t* pointer, uint32_t pixel_type, int64_t value) {
  const uint64_t raw = static_cast<uint64_t>(value);
  const size_t width = raster_resident_pixel_width(pixel_type);
  for (size_t byte = 0; byte < width; ++byte)
    pointer[byte] = static_cast<uint8_t>((raw >> (byte * 8)) & 0xffu);
}

inline bool raster_resident_positive_zero(double value) {
  return sycl::bit_cast<uint64_t>(value) == 0;
}

inline bool raster_resident_row_is_canonical_null(const pgaccel_resident_raster_row& row) {
  return row.width == 0 && row.height == 0 && row.first_band == 0 && row.band_count == 0 &&
         row.srid == 0 && row.flags == 0 && raster_resident_positive_zero(row.scale_x) &&
         raster_resident_positive_zero(row.scale_y) && raster_resident_positive_zero(row.ip_x) &&
         raster_resident_positive_zero(row.ip_y) && raster_resident_positive_zero(row.skew_x) &&
         raster_resident_positive_zero(row.skew_y);
}

constexpr uint32_t RASTER_RESIDENT_FAILURE_VIEW = 1u << 0;
constexpr uint32_t RASTER_RESIDENT_FAILURE_RULES = 1u << 1;
constexpr uint32_t RASTER_RESIDENT_FAILURE_OFFSETS = 1u << 2;
constexpr uint32_t RASTER_RESIDENT_FAILURE_CAPACITY = 1u << 3;
constexpr uint32_t RASTER_RESIDENT_FAILURE_BYTE_BUDGET = 1u << 4;
constexpr uint32_t RASTER_RESIDENT_FAILURE_NUMERIC = 1u << 5;
static_assert(RASTER_RESIDENT_FAILURE_VIEW == PGACCEL_RASTER_VALIDATION_VIEW);
static_assert(RASTER_RESIDENT_FAILURE_RULES == PGACCEL_RASTER_VALIDATION_RULES);
static_assert(RASTER_RESIDENT_FAILURE_OFFSETS == PGACCEL_RASTER_VALIDATION_OFFSETS);
static_assert(RASTER_RESIDENT_FAILURE_CAPACITY == PGACCEL_RASTER_VALIDATION_CAPACITY);
static_assert(RASTER_RESIDENT_FAILURE_BYTE_BUDGET == PGACCEL_RASTER_VALIDATION_BYTE_BUDGET);
static_assert(RASTER_RESIDENT_FAILURE_NUMERIC == PGACCEL_RASTER_VALIDATION_NUMERIC_OVERFLOW);
static_assert(sizeof(size_t) == sizeof(uint64_t), "resident raster ABI requires LP64 size_t");
constexpr size_t RASTER_RESIDENT_MAX_ROW_VALIDATION_LAUNCH =
    PGACCEL_RESIDENT_RASTER_ROWS_PER_VALIDATION_LAUNCH;

class RasterResidentRuleValidationKernel;
class RasterResidentRowValidationKernel;
class RasterResidentLowBitValidationKernel;
class RasterResidentRowActionKernel;
class RasterResidentReclassKernel;

}  // namespace

extern "C" pgaccel_status
pgaccel_raster_reclass_resident_ex(const pgaccel_raster_reclass_resident_request* request,
                                   int32_t* detail) try {
  if (detail == nullptr)
    return PGACCEL_INVALID_ARGUMENT;
  *detail = PGACCEL_RASTER_DETAIL_NONE;
  if (request == nullptr) {
    *detail = PGACCEL_RASTER_DETAIL_CONTRACT;
    return PGACCEL_INVALID_ARGUMENT;
  }
  const pgaccel_resident_raster_view& view = request->input;
  if (request->abi_version != PGACCEL_RESIDENT_RASTER_ABI_VERSION || request->flags != 0 ||
      request->pad != 0 || view.abi_version != PGACCEL_RESIDENT_RASTER_ABI_VERSION ||
      view.flags != 0 || request->first_row > view.row_count ||
      request->count > view.row_count - request->first_row) {
    *detail = PGACCEL_RASTER_DETAIL_CONTRACT;
    return PGACCEL_INVALID_ARGUMENT;
  }
  if (request->count == 0)
    return PGACCEL_OK;

  int64_t output_minimum = 0;
  int64_t output_maximum = 0;
  const size_t output_width = raster_resident_pixel_width(request->output_pixel_type);
  if (!raster_resident_integer_bounds(request->output_pixel_type, &output_minimum,
                                      &output_maximum) ||
      output_width == 0) {
    *detail = PGACCEL_RASTER_DETAIL_CONTRACT;
    return PGACCEL_INVALID_ARGUMENT;
  }

  size_t expected_rows_bytes = 0;
  size_t expected_bands_bytes = 0;
  size_t band_offset_count = 0;
  size_t expected_band_offsets_bytes = 0;
  size_t expected_rules_bytes = 0;
  size_t output_offset_count = 0;
  size_t expected_output_offsets_bytes = 0;
  size_t expected_total_output_bytes = 0;
  if (!raster_resident_checked_bytes(view.row_count, sizeof(pgaccel_resident_raster_row),
                                     &expected_rows_bytes) ||
      !raster_resident_checked_bytes(view.band_count, sizeof(pgaccel_resident_raster_band),
                                     &expected_bands_bytes) ||
      view.band_count == std::numeric_limits<size_t>::max() ||
      (band_offset_count = view.band_count + 1,
       !raster_resident_checked_bytes(band_offset_count, sizeof(uint64_t),
                                      &expected_band_offsets_bytes)) ||
      !raster_resident_checked_bytes(request->rule_count,
                                     sizeof(pgaccel_resident_raster_reclass_rule),
                                     &expected_rules_bytes) ||
      request->count == std::numeric_limits<size_t>::max() ||
      (output_offset_count = request->count + 1,
       !raster_resident_checked_bytes(output_offset_count, sizeof(uint64_t),
                                      &expected_output_offsets_bytes)) ||
      !raster_resident_checked_bytes(request->max_total_pixels, output_width,
                                     &expected_total_output_bytes)) {
    *detail = PGACCEL_RASTER_DETAIL_NUMERIC_OVERFLOW;
    return PGACCEL_INVALID_ARGUMENT;
  }

  if (view.rows_bytes != expected_rows_bytes || view.bands_bytes != expected_bands_bytes ||
      view.band_offsets_bytes != expected_band_offsets_bytes ||
      (view.nulls_bytes != 0 && view.nulls_bytes != view.row_count) || request->rule_count == 0 ||
      request->rule_count > PGACCEL_RESIDENT_RASTER_MAX_RECLASS_RULES ||
      request->rules_bytes != expected_rules_bytes ||
      request->output_offsets_bytes != expected_output_offsets_bytes ||
      request->row_actions_bytes != request->count ||
      request->validation_scratch_bytes != sizeof(pgaccel_resident_raster_validation_scratch) ||
      request->max_chunk_pixels == 0 ||
      (request->max_total_pixels != 0 && request->max_chunk_pixels > request->max_total_pixels)) {
    *detail = PGACCEL_RASTER_DETAIL_CONTRACT;
    return PGACCEL_INVALID_ARGUMENT;
  }
  if (expected_total_output_bytes > request->output_pixels_bytes) {
    *detail = PGACCEL_RASTER_DETAIL_CAPACITY;
    return PGACCEL_INVALID_ARGUMENT;
  }
  if (!raster_resident_launch_count_within_limit(request->max_total_pixels,
                                                 request->max_chunk_pixels) ||
      !raster_resident_launch_count_within_limit(request->count,
                                                 RASTER_RESIDENT_MAX_ROW_VALIDATION_LAUNCH)) {
    *detail = PGACCEL_RASTER_DETAIL_BYTE_BUDGET;
    return PGACCEL_INVALID_ARGUMENT;
  }

  auto aligned_pointer = [](const void* pointer, size_t alignment) {
    return pointer == nullptr || reinterpret_cast<uintptr_t>(pointer) % alignment == 0;
  };
  if (!aligned_pointer(view.band_offsets, alignof(uint64_t)) ||
      !aligned_pointer(view.rows, alignof(pgaccel_resident_raster_row)) ||
      !aligned_pointer(view.bands, alignof(pgaccel_resident_raster_band)) ||
      !aligned_pointer(request->rules, alignof(pgaccel_resident_raster_reclass_rule)) ||
      !aligned_pointer(request->output_offsets, alignof(uint64_t)) ||
      !aligned_pointer(request->validation_scratch,
                       alignof(pgaccel_resident_raster_validation_scratch))) {
    *detail = PGACCEL_RASTER_DETAIL_CONTRACT;
    return PGACCEL_INVALID_ARGUMENT;
  }

  RasterResidentSpan spans[10]{};
  size_t span_count = 0;
  auto add_span = [&](const void* pointer, size_t bytes) {
    RasterResidentSpan span{};
    if (!raster_resident_exact_span(pointer, bytes, &span))
      return false;
    if (span.active)
      spans[span_count++] = span;
    return true;
  };
  if (!add_span(view.pixels, view.pixels_bytes) ||
      !add_span(view.band_offsets, view.band_offsets_bytes) ||
      !add_span(view.rows, view.rows_bytes) || !add_span(view.bands, view.bands_bytes) ||
      !add_span(view.nulls, view.nulls_bytes) || !add_span(request->rules, request->rules_bytes) ||
      !add_span(request->output_offsets, request->output_offsets_bytes) ||
      !add_span(request->output_pixels, request->output_pixels_bytes) ||
      !add_span(request->row_actions, request->row_actions_bytes) ||
      !add_span(request->validation_scratch, request->validation_scratch_bytes)) {
    *detail = PGACCEL_RASTER_DETAIL_CONTRACT;
    return PGACCEL_INVALID_ARGUMENT;
  }
  for (size_t left = 0; left < span_count; ++left) {
    for (size_t right = left + 1; right < span_count; ++right) {
      if (raster_resident_spans_overlap(spans[left], spans[right])) {
        *detail = PGACCEL_RASTER_DETAIL_CONTRACT;
        return PGACCEL_INVALID_ARGUMENT;
      }
    }
  }

  sycl::queue& queue = get_queue();
  for (size_t span = 0; span < span_count; ++span) {
    if (!raster_resident_current_device_pointer(queue,
                                                reinterpret_cast<const void*>(spans[span].begin))) {
      *detail = PGACCEL_RASTER_DETAIL_CONTRACT;
      return PGACCEL_INVALID_ARGUMENT;
    }
  }

  auto* validation = request->validation_scratch;
  queue.memset(validation, 0, sizeof(*validation));

  const pgaccel_resident_raster_view input = view;
  const auto* rules = request->rules;
  const auto* output_offsets = request->output_offsets;
  auto* output_pixels = request->output_pixels;
  auto* row_actions = request->row_actions;
  const size_t rule_count = request->rule_count;
  const int64_t minimum = output_minimum;
  const int64_t maximum = output_maximum;
  queue.parallel_for<RasterResidentRuleValidationKernel>(
      sycl::range<1>(rule_count), [=](sycl::id<1> id) {
        const size_t index = id[0];
        const pgaccel_resident_raster_reclass_rule rule = rules[index];
        const bool invalid = rule.source < static_cast<int64_t>(INT32_MIN) ||
                             rule.source > static_cast<int64_t>(UINT32_MAX) ||
                             rule.destination < minimum || rule.destination > maximum ||
                             (index > 0 && rules[index - 1].source >= rule.source);
        if (invalid) {
          sycl::atomic_ref<uint32_t, sycl::memory_order::relaxed, sycl::memory_scope::device,
                           sycl::access::address_space::global_space>
              failures(validation->failures);
          failures.fetch_or(RASTER_RESIDENT_FAILURE_RULES);
        }
      });

  const size_t selected_first_row = request->first_row;
  const size_t selected_count = request->count;
  const size_t output_capacity = request->output_pixels_bytes;
  const uint32_t output_type = request->output_pixel_type;
  const size_t output_element_bytes = output_width;
  const uint32_t output_element_shift = raster_resident_width_shift(output_width);
  const size_t exact_total_output_bytes = expected_total_output_bytes;
  for (size_t launch_start = 0; launch_start < selected_count;) {
    const size_t launch_count =
        std::min(RASTER_RESIDENT_MAX_ROW_VALIDATION_LAUNCH, selected_count - launch_start);
    queue.parallel_for<RasterResidentRowValidationKernel>(
        sycl::range<1>(launch_count), [=](sycl::id<1> id) {
          const size_t local_row = launch_start + id[0];
          const size_t row_index = selected_first_row + local_row;
          const pgaccel_resident_raster_row row = input.rows[row_index];
          const uint8_t null_byte = input.nulls == nullptr ? 0 : input.nulls[row_index];
          const uint64_t output_start = output_offsets[local_row];
          const uint64_t output_end = output_offsets[local_row + 1];
          uint32_t row_failure = 0;
          if (local_row == 0)
            validation->first_output_offset = output_start;
          if (local_row + 1 == selected_count) {
            validation->last_output_offset = output_end;
            const uint64_t first_output = output_offsets[0];
            if (output_end < first_output || output_end - first_output != exact_total_output_bytes)
              row_failure |= RASTER_RESIDENT_FAILURE_BYTE_BUDGET;
          }

          if (output_start > output_end || output_start % output_element_bytes != 0 ||
              output_end % output_element_bytes != 0)
            row_failure |= RASTER_RESIDENT_FAILURE_OFFSETS;
          if (output_start > output_capacity || output_end > output_capacity)
            row_failure |= RASTER_RESIDENT_FAILURE_CAPACITY;
          if (local_row == 0 && (input.band_offsets[0] != 0 ||
                                 input.band_offsets[input.band_count] != input.pixels_bytes))
            row_failure |= RASTER_RESIDENT_FAILURE_VIEW;

          uint64_t expected_output_bytes = 0;
          if (null_byte > 1) {
            row_failure |= RASTER_RESIDENT_FAILURE_VIEW;
          } else if (null_byte != 0) {
            if (!raster_resident_row_is_canonical_null(row))
              row_failure |= RASTER_RESIDENT_FAILURE_VIEW;
          } else {
            const bool finite_metadata = sycl::isfinite(row.scale_x) &&
                                         sycl::isfinite(row.scale_y) && sycl::isfinite(row.ip_x) &&
                                         sycl::isfinite(row.ip_y) && sycl::isfinite(row.skew_x) &&
                                         sycl::isfinite(row.skew_y);
            const uint64_t first_band = row.first_band;
            const uint64_t band_end = first_band + row.band_count;
            if (row.flags != 0 || row.srid < 0 || row.srid > 999999 || !finite_metadata ||
                first_band > input.band_count || band_end > input.band_count) {
              row_failure |= RASTER_RESIDENT_FAILURE_VIEW;
            } else if (row.band_count != 0) {
              const pgaccel_resident_raster_band band = input.bands[first_band];
              const size_t input_element_bytes = raster_resident_pixel_width(band.pixel_type);
              const uint64_t input_start = input.band_offsets[first_band];
              const uint64_t input_end = input.band_offsets[first_band + 1];
              const uint64_t pixel_count = static_cast<uint64_t>(row.width) * row.height;
              const uint32_t known_band_flags =
                  PGACCEL_RESIDENT_RASTER_BAND_HAS_NODATA | PGACCEL_RESIDENT_RASTER_BAND_IS_NODATA;
              const uint64_t max_bytes = std::numeric_limits<uint64_t>::max();
              const uint32_t input_element_shift =
                  raster_resident_width_shift(input_element_bytes == 0 ? 1 : input_element_bytes);
              const bool numeric_overflow =
                  input_element_bytes != 0 && (pixel_count > (max_bytes >> input_element_shift) ||
                                               pixel_count > (max_bytes >> output_element_shift));
              if (numeric_overflow) {
                row_failure |= RASTER_RESIDENT_FAILURE_NUMERIC;
              } else if (input_element_bytes == 0 || (band.flags & ~known_band_flags) != 0 ||
                         ((band.flags & PGACCEL_RESIDENT_RASTER_BAND_IS_NODATA) != 0 &&
                          (band.flags & PGACCEL_RESIDENT_RASTER_BAND_HAS_NODATA) == 0) ||
                         ((band.flags & PGACCEL_RESIDENT_RASTER_BAND_HAS_NODATA) == 0 &&
                          !raster_resident_positive_zero(band.nodata)) ||
                         input_start > input_end || input_end > input.pixels_bytes) {
                row_failure |= RASTER_RESIDENT_FAILURE_VIEW;
              } else {
                const uint64_t expected_input_bytes = pixel_count << input_element_shift;
                expected_output_bytes = pixel_count << output_element_shift;
                if (input_end - input_start != expected_input_bytes)
                  row_failure |= RASTER_RESIDENT_FAILURE_VIEW;
              }
            }
          }
          if (output_start <= output_end && output_end - output_start != expected_output_bytes)
            row_failure |= RASTER_RESIDENT_FAILURE_OFFSETS;
          if (row_failure != 0) {
            sycl::atomic_ref<uint32_t, sycl::memory_order::relaxed, sycl::memory_scope::device,
                             sycl::access::address_space::global_space>
                failures(validation->failures);
            failures.fetch_or(row_failure);
          }
        });
    launch_start += launch_count;
  }
  const size_t total_pixels = request->max_total_pixels;
  const size_t max_chunk_pixels = request->max_chunk_pixels;
  for (size_t launch_start = 0; launch_start < total_pixels;) {
    const size_t launch_count = std::min(max_chunk_pixels, total_pixels - launch_start);
    queue.parallel_for<RasterResidentLowBitValidationKernel>(
        sycl::range<1>(launch_count), [=](sycl::id<1> id) {
          if (validation->failures != 0)
            return;
          const uint64_t pixel_index = launch_start + id[0];
          const uint64_t output_byte = output_offsets[0] + (pixel_index << output_element_shift);

          size_t low = 0;
          size_t high = selected_count + 1;
          while (low < high) {
            const size_t middle = low + (high - low) / 2;
            if (output_offsets[middle] <= output_byte)
              low = middle + 1;
            else
              high = middle;
          }
          const size_t local_row = low - 1;
          const pgaccel_resident_raster_row row = input.rows[selected_first_row + local_row];
          const pgaccel_resident_raster_band band = input.bands[row.first_band];
          uint8_t allowed_bits = 0xff;
          if (band.pixel_type == PGACCEL_RESIDENT_RASTER_BOOL)
            allowed_bits = 0x01;
          else if (band.pixel_type == PGACCEL_RESIDENT_RASTER_UINT2)
            allowed_bits = 0x03;
          else if (band.pixel_type == PGACCEL_RESIDENT_RASTER_UINT4)
            allowed_bits = 0x0f;
          if (allowed_bits != 0xff) {
            const uint64_t row_pixel =
                (output_byte - output_offsets[local_row]) / output_element_bytes;
            const uint64_t input_byte = input.band_offsets[row.first_band] + row_pixel;
            if ((input.pixels[input_byte] & ~allowed_bits) != 0) {
              sycl::atomic_ref<uint32_t, sycl::memory_order::relaxed, sycl::memory_scope::device,
                               sycl::access::address_space::global_space>
                  failures(validation->failures);
              failures.fetch_or(RASTER_RESIDENT_FAILURE_VIEW);
            }
          }
        });
    launch_start += launch_count;
  }

  for (size_t launch_start = 0; launch_start < selected_count;) {
    const size_t launch_count =
        std::min(RASTER_RESIDENT_MAX_ROW_VALIDATION_LAUNCH, selected_count - launch_start);
    queue.parallel_for<RasterResidentRowActionKernel>(
        sycl::range<1>(launch_count), [=](sycl::id<1> id) {
          if (validation->failures != 0)
            return;
          const size_t local_row = launch_start + id[0];
          const size_t row_index = selected_first_row + local_row;
          if (input.nulls != nullptr && input.nulls[row_index] != 0)
            row_actions[local_row] = PGACCEL_RASTER_ROW_NULL;
          else if (input.rows[row_index].band_count == 0)
            row_actions[local_row] = PGACCEL_RASTER_ROW_PASSTHROUGH;
          else
            row_actions[local_row] = PGACCEL_RASTER_ROW_RECLASSIFIED;
        });
    launch_start += launch_count;
  }

  for (size_t launch_start = 0; launch_start < total_pixels;) {
    const size_t launch_count = std::min(max_chunk_pixels, total_pixels - launch_start);
    queue.parallel_for<RasterResidentReclassKernel>(
        sycl::range<1>(launch_count), [=](sycl::id<1> id) {
          if (validation->failures != 0)
            return;
          const uint64_t pixel_index = launch_start + id[0];
          const uint64_t output_byte = output_offsets[0] + (pixel_index << output_element_shift);

          size_t low = 0;
          size_t high = selected_count + 1;
          while (low < high) {
            const size_t middle = low + (high - low) / 2;
            if (output_offsets[middle] <= output_byte)
              low = middle + 1;
            else
              high = middle;
          }
          const size_t local_row = low - 1;
          const pgaccel_resident_raster_row row = input.rows[selected_first_row + local_row];
          const pgaccel_resident_raster_band band = input.bands[row.first_band];
          const size_t input_element_bytes = raster_resident_pixel_width(band.pixel_type);
          const uint32_t input_element_shift = raster_resident_width_shift(input_element_bytes);
          const uint64_t row_pixel =
              (output_byte - output_offsets[local_row]) / output_element_bytes;
          const uint64_t input_byte =
              input.band_offsets[row.first_band] + (row_pixel << input_element_shift);
          const double value =
              (band.flags & PGACCEL_RESIDENT_RASTER_BAND_IS_NODATA) != 0
                  ? band.nodata
                  : raster_resident_read_pixel(input.pixels + input_byte, band.pixel_type);

          int64_t destination = 0;
          constexpr double kPostgisFltEpsilon = 1.1920928955078125e-7;
          for (size_t rule_index = 0; rule_index < rule_count; ++rule_index) {
            const double source = static_cast<double>(rules[rule_index].source);
            if (source == value || sycl::fabs(source - value) <= kPostgisFltEpsilon) {
              destination = rules[rule_index].destination;
              break;
            }
          }
          raster_resident_write_integer(output_pixels + output_byte, output_type, destination);
        });
    launch_start += launch_count;
  }
  queue.wait_and_throw();
  pgaccel_record_gpu_exec();
  return PGACCEL_OK;
} catch (const pgaccel_no_device_error&) {
  return PGACCEL_ERROR_NO_DEVICE;
} catch (const std::bad_alloc&) {
  return PGACCEL_OOM;
} catch (const std::exception& error) {
  return pgaccel_kernel_failure("pgaccel_raster_reclass_resident_ex", &error);
} catch (...) {
  return pgaccel_kernel_failure("pgaccel_raster_reclass_resident_ex", nullptr);
}

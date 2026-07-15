// bbox_ops_f64.cpp - Bulk fp64 bounding box intersection kernel.
//
// AdaptiveCpp's Metal emitter cannot lower the direct soft-fp64 comparison
// kernel for this entry point. Bounding-box intersection only needs IEEE-754
// ordering, so the device kernel compares the raw binary64 encodings instead.
// This keeps the work on the GPU without invoking Metal's fp64 emitter.

#include <sycl/sycl.hpp>

#include <cstddef>
#include <cstdint>
#include <limits>
#include <stdexcept>

#include "pgaccel_ffi.h"
#include "pgaccel_queue.h"

namespace {

constexpr size_t kBoxWords = 4;
constexpr uint64_t kF64Sign = UINT64_C(0x8000000000000000);
constexpr uint64_t kF64Exponent = UINT64_C(0x7ff0000000000000);
constexpr uint64_t kF64Fraction = UINT64_C(0x000fffffffffffff);
static_assert(sizeof(double) == sizeof(uint64_t) && std::numeric_limits<double>::is_iec559,
              "fp64 bbox requires IEEE-754 binary64 doubles");

static bool bbox_counts_fit(size_t count_a, size_t count_b) {
  const size_t max = std::numeric_limits<size_t>::max();
  if (count_a > max / kBoxWords || count_b > max / kBoxWords) {
    return false;
  }

  const size_t words_a = count_a * kBoxWords;
  const size_t words_b = count_b * kBoxWords;
  if (words_a > max / sizeof(uint64_t) || words_b > max / sizeof(uint64_t)) {
    return false;
  }

  return count_b == 0 || count_a <= max / count_b;
}

static inline bool f64_bits_is_nan(uint64_t bits) {
  return (bits & kF64Exponent) == kF64Exponent && (bits & kF64Fraction) != 0;
}

// IEEE ordered-less over binary64 encodings. NaNs are unordered, and both
// signed zero encodings compare equal, matching the source-level double
// comparisons used by PostgreSQL's bbox prefilter.
static inline bool f64_bits_ordered_less(uint64_t lhs, uint64_t rhs) {
  if (f64_bits_is_nan(lhs) || f64_bits_is_nan(rhs)) {
    return false;
  }

  const uint64_t lhs_magnitude = lhs & ~kF64Sign;
  const uint64_t rhs_magnitude = rhs & ~kF64Sign;
  if (lhs_magnitude == 0 && rhs_magnitude == 0) {
    return false;
  }

  const bool lhs_negative = (lhs & kF64Sign) != 0;
  const bool rhs_negative = (rhs & kF64Sign) != 0;
  if (lhs_negative != rhs_negative) {
    return lhs_negative;
  }
  return lhs_negative ? lhs > rhs : lhs < rhs;
}

static pgaccel_status bbox_intersects_bulk_sycl_f64(sycl::queue& queue, const double* boxes_a,
                                                    size_t count_a, const double* boxes_b,
                                                    size_t count_b, uint8_t* result,
                                                    size_t* hit_count) {
  if (!bbox_counts_fit(count_a, count_b)) {
    return PGACCEL_ERROR;
  }

  const size_t words_a = count_a * kBoxWords;
  const size_t words_b = count_b * kBoxWords;
  const size_t total_pairs = count_a * count_b;
  uint64_t* device_a = sycl::malloc_device<uint64_t>(words_a, queue);
  uint64_t* device_b = sycl::malloc_device<uint64_t>(words_b, queue);
  uint8_t* device_result = sycl::malloc_device<uint8_t>(total_pairs, queue);
  size_t* device_hits = hit_count == nullptr ? nullptr : sycl::malloc_device<size_t>(1, queue);

  if (device_a == nullptr || device_b == nullptr || device_result == nullptr ||
      (hit_count != nullptr && device_hits == nullptr)) {
    sycl::free(device_a, queue);
    sycl::free(device_b, queue);
    sycl::free(device_result, queue);
    sycl::free(device_hits, queue);
    return PGACCEL_OOM;
  }

  try {
    // Copy the binary64 object representations verbatim. Device code never
    // materializes a double, avoiding the broken Metal fp64 lowering path.
    queue.memcpy(device_a, boxes_a, words_a * sizeof(uint64_t));
    queue.memcpy(device_b, boxes_b, words_b * sizeof(uint64_t));
    queue.wait_and_throw();

    const size_t inner_count = count_b;
    const sycl::event pair_event = queue.submit([&](sycl::handler& handler) {
      handler.parallel_for(sycl::range<1>(total_pairs), [=](sycl::id<1> id) {
        const size_t pair_index = id[0];
        const size_t outer_index = pair_index / inner_count;
        const size_t inner_index = pair_index % inner_count;
        const uint64_t* outer = device_a + outer_index * kBoxWords;
        const uint64_t* inner = device_b + inner_index * kBoxWords;

        const bool separated = f64_bits_ordered_less(outer[2], inner[0]) ||
                               f64_bits_ordered_less(inner[2], outer[0]) ||
                               f64_bits_ordered_less(outer[3], inner[1]) ||
                               f64_bits_ordered_less(inner[3], outer[1]);
        device_result[pair_index] = separated ? 0 : 1;
      });
    });

    sycl::event terminal_event = pair_event;
    if (device_hits != nullptr) {
      terminal_event = queue.submit([&](sycl::handler& handler) {
        handler.depends_on(pair_event);
        handler.single_task([=]() {
          size_t hits = 0;
          for (size_t pair_index = 0; pair_index < total_pairs; ++pair_index) {
            hits += device_result[pair_index] != 0 ? 1 : 0;
          }
          *device_hits = hits;
        });
      });
    }
    terminal_event.wait_and_throw();

    queue.memcpy(result, device_result, total_pairs * sizeof(uint8_t));
    if (device_hits != nullptr) {
      queue.memcpy(hit_count, device_hits, sizeof(size_t));
    }
    queue.wait_and_throw();
  } catch (...) {
    sycl::free(device_a, queue);
    sycl::free(device_b, queue);
    sycl::free(device_result, queue);
    sycl::free(device_hits, queue);
    throw;
  }

  sycl::free(device_a, queue);
  sycl::free(device_b, queue);
  sycl::free(device_result, queue);
  sycl::free(device_hits, queue);
  return PGACCEL_OK;
}

}  // namespace

extern "C" pgaccel_status pgaccel_bbox_intersects_bulk_f64(const double* boxes_a, size_t count_a,
                                                           const double* boxes_b, size_t count_b,
                                                           uint8_t* result, size_t* hit_count) {
  if (count_a == 0 || count_b == 0) {
    if (hit_count != nullptr) {
      *hit_count = 0;
    }
    return PGACCEL_OK;
  }
  if (boxes_a == nullptr || boxes_b == nullptr || result == nullptr) {
    return PGACCEL_ERROR;
  }
  if (!bbox_counts_fit(count_a, count_b)) {
    return PGACCEL_ERROR;
  }

  try {
    sycl::queue& queue = pgaccel_require_queue();
    const pgaccel_status status =
        bbox_intersects_bulk_sycl_f64(queue, boxes_a, count_a, boxes_b, count_b, result, hit_count);
    if (status == PGACCEL_OK) {
      pgaccel_record_gpu_exec();
    }
    return status;
  } catch (const pgaccel_no_device_error&) {
    return PGACCEL_ERROR_NO_DEVICE;
  } catch (const std::exception& error) {
    return pgaccel_kernel_failure(__func__, &error);
  } catch (...) {
    return pgaccel_kernel_failure(__func__, nullptr);
  }
}

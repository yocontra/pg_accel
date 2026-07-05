// bbox_ops_f64.cpp — Bulk fp64 bounding box intersection fallback.
//
// AdaptiveCpp's Metal emitter currently rejects this otherwise-simple fp64
// bbox SYCL kernel with an indirect-call lowering error. Worse, the failed
// fp64 kernel poisons the fp32 bbox code object when both are linked into the
// same target. Keep fp64 bbox correct and available through a host fallback;
// the fp32 BOX2DF path remains GPU accelerated in bbox_ops.cpp.

#include <cstddef>
#include <cstdint>
#include <limits>

#include "pgaccel_ffi.h"

namespace {

static bool bbox_counts_fit(size_t count_a, size_t count_b) {
  const size_t max = std::numeric_limits<size_t>::max();
  if (count_a > max / 4 || count_b > max / 4) {
    return false;
  }
  return count_b == 0 || count_a <= max / count_b;
}

}  // namespace

extern "C" pgaccel_status pgaccel_bbox_intersects_bulk_f64(const double* boxes_a, size_t count_a,
                                                           const double* boxes_b, size_t count_b,
                                                           uint8_t* result, size_t* hit_count) {
  if (count_a == 0 || count_b == 0) {
    if (hit_count)
      *hit_count = 0;
    return PGACCEL_OK;
  }

  if (!boxes_a || !boxes_b || !result) {
    return PGACCEL_ERROR;
  }

  if (!bbox_counts_fit(count_a, count_b)) {
    return PGACCEL_ERROR;
  }

  size_t hits = 0;
  for (size_t i = 0; i < count_a; ++i) {
    const double a_xmin = boxes_a[i * 4 + 0];
    const double a_ymin = boxes_a[i * 4 + 1];
    const double a_xmax = boxes_a[i * 4 + 2];
    const double a_ymax = boxes_a[i * 4 + 3];

    for (size_t j = 0; j < count_b; ++j) {
      const double b_xmin = boxes_b[j * 4 + 0];
      const double b_ymin = boxes_b[j * 4 + 1];
      const double b_xmax = boxes_b[j * 4 + 2];
      const double b_ymax = boxes_b[j * 4 + 3];

      // NaN comparisons are false, so any NaN coordinate conservatively
      // survives the bbox prefilter instead of producing a false negative.
      const bool intersects =
          !(a_xmax < b_xmin || a_xmin > b_xmax || a_ymax < b_ymin || a_ymin > b_ymax);
      const size_t idx = i * count_b + j;
      result[idx] = intersects ? 1 : 0;
      hits += intersects ? 1 : 0;
    }
  }

  if (hit_count) {
    *hit_count = hits;
  }

  return PGACCEL_OK;
}

#pragma once

#include <cstddef>
#include <cstdint>

#include "pgaccel_ffi.h"

namespace pgaccel_raster_detail {

// A launch-level policy, never a per-pixel branch.
inline constexpr size_t linear_rule_limit = 16;

inline bool reclass_matches(int64_t source_integer, double value) {
  constexpr double epsilon = 1.1920928955078125e-7;
  const double source = static_cast<double>(source_integer);
  return source == value || __builtin_fabs(source - value) <= epsilon;
}

inline int64_t reclass_lookup_linear(const pgaccel_resident_raster_reclass_rule* rules,
                                     size_t count, double value) {
  for (size_t index = 0; index < count; ++index)
    if (reclass_matches(rules[index].source, value))
      return rules[index].destination;
  return 0;
}

// The caller chooses this specialization only above linear_rule_limit. The
// device rule-validation pass proves strictly increasing integer sources
// within [INT32_MIN, UINT32_MAX]. They convert to double without loss. Searching
// by the unmodified pixel value avoids rounding a value +/- epsilon at large
// magnitudes. Only the predecessor and lower-bound source can match FLT_EQ.
// Inspect them in source order to preserve the original first-match policy.
inline int64_t reclass_lookup_indexed(const pgaccel_resident_raster_reclass_rule* rules,
                                      size_t count, double value) {
  // Endpoint checks retain cheap first/last hits and out-of-domain misses.
  // Integer sources are separated by >= 1, much more than 2*FLT_EPSILON,
  // so an endpoint match cannot take precedence over another matching rule.
  if (reclass_matches(rules[0].source, value))
    return rules[0].destination;
  if (value < static_cast<double>(rules[0].source))
    return 0;
  if (reclass_matches(rules[count - 1].source, value))
    return rules[count - 1].destination;
  if (value > static_cast<double>(rules[count - 1].source))
    return 0;
  size_t low = 1;
  size_t high = count - 1;
  while (low < high) {
    const size_t middle = low + (high - low) / 2;
    if (static_cast<double>(rules[middle].source) < value)
      low = middle + 1;
    else
      high = middle;
  }
  const size_t begin = low == 0 ? 0 : low - 1;
  const size_t end = low < count ? low + 1 : count;
  for (size_t index = begin; index < end; ++index) {
    if (reclass_matches(rules[index].source, value))
      return rules[index].destination;
  }
  return 0;
}

}  // namespace pgaccel_raster_detail

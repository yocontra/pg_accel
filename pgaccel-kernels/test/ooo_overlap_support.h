#ifndef PGACCEL_TEST_OOO_OVERLAP_SUPPORT_H
#define PGACCEL_TEST_OOO_OVERLAP_SUPPORT_H

#include <stdbool.h>
#include <stddef.h>
#include <stdint.h>

#include "pgaccel_ffi.h"

typedef struct {
  uint64_t serial_wall_ns;
  uint64_t overlap_wall_ns;
  uint64_t reduce_start_ns;
  uint64_t reduce_end_ns;
  uint64_t resident_start_ns;
  uint64_t resident_end_ns;
  uint64_t final_start_ns;
  uint64_t final_end_ns;
  bool spans_overlap;
  bool wall_time_improved;
} pgaccel_ooo_overlap_report;

#ifdef __cplusplus
extern "C" {
#endif

pgaccel_status pgaccel_resident_reduce_overlap_probe(size_t count, uint32_t spin_iters,
                                                     pgaccel_ooo_overlap_report* out);

#ifdef __cplusplus
}
#endif

#endif

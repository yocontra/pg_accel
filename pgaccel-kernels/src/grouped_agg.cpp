/*
 * Dark Phase-4A link surface for the frozen grouped-aggregate ABI.
 * Phase 4B replaces these bodies with descriptor validation and SYCL kernels.
 * No runtime caller is wired to these symbols during Phase 4A.
 */

#include "pgaccel_olap.h"

extern "C" pgaccel_status pgaccel_grouped_agg_workspace_requirements(
    const pgaccel_grouped_agg_desc* desc, pgaccel_grouped_agg_workspace_req* out) {
  (void)desc;
  (void)out;
  return PGACCEL_UNSUPPORTED;
}

extern "C" pgaccel_status pgaccel_grouped_agg_execute(const pgaccel_grouped_agg_desc* desc,
                                                       pgaccel_grouped_agg_out* out) {
  (void)desc;
  (void)out;
  return PGACCEL_UNSUPPORTED;
}

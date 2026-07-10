#include "pgaccel_olap.h"

#include <sycl/sycl.hpp>

#include <cstddef>
#include <cstdio>
#include <exception>

#include "pgaccel_queue.h"

namespace {

bool valid_alignment(size_t alignment) {
  return alignment != 0 && (alignment & (alignment - 1)) == 0;
}

}  // namespace

extern "C" pgaccel_status pgaccel_grouped_agg_workspace_alloc(size_t bytes, size_t alignment,
                                                               int32_t space, void** out) try {
  if (out == nullptr)
    return PGACCEL_ERROR;
  *out = nullptr;
  if (!valid_alignment(alignment))
    return PGACCEL_ERROR;
  if (space != PGACCEL_MEM_SPACE_SHARED_USM && space != PGACCEL_MEM_SPACE_DEVICE)
    return PGACCEL_ERROR;
  if (bytes == 0)
    return PGACCEL_OK;

  const pgaccel_status init_status = pgaccel_init();
  if (init_status != PGACCEL_OK)
    return init_status;
  sycl::queue* q = pgaccel_get_queue();
  if (q == nullptr)
    return PGACCEL_ERROR_NO_DEVICE;

  const size_t effective_alignment =
      alignment < alignof(void*) ? alignof(void*) : alignment;
  void* ptr = nullptr;
  if (space == PGACCEL_MEM_SPACE_SHARED_USM)
    ptr = sycl::aligned_alloc_shared(effective_alignment, bytes, *q);
  else
    ptr = sycl::aligned_alloc_device(effective_alignment, bytes, *q);
  if (ptr == nullptr)
    return PGACCEL_OOM;
  *out = ptr;
  return PGACCEL_OK;
} catch (const pgaccel_no_device_error&) {
  return PGACCEL_ERROR_NO_DEVICE;
} catch (const sycl::exception& e) {
  std::fprintf(stderr, "pgaccel: grouped workspace allocation failed: %s\n", e.what());
  return PGACCEL_ERROR;
} catch (const std::exception& e) {
  return pgaccel_kernel_failure("pgaccel_grouped_agg_workspace_alloc", &e);
} catch (...) {
  return pgaccel_kernel_failure("pgaccel_grouped_agg_workspace_alloc", nullptr);
}

extern "C" void pgaccel_grouped_agg_workspace_free(void* ptr) {
  if (ptr == nullptr)
    return;
  const pgaccel_status init_status = pgaccel_init();
  if (init_status != PGACCEL_OK) {
    std::fprintf(stderr, "pgaccel: %s: runtime init failed with status %d; allocation leaked\n",
                 __func__, static_cast<int>(init_status));
    return;
  }
  sycl::queue* q = pgaccel_get_queue();
  if (q == nullptr) {
    std::fprintf(stderr, "pgaccel: %s: runtime queue unavailable; allocation leaked\n", __func__);
    return;
  }
  try {
    sycl::free(ptr, *q);
  } catch (const std::exception& e) {
    std::fprintf(stderr, "pgaccel: %s: sycl::free failed: %s\n", __func__, e.what());
  } catch (...) {
    std::fprintf(stderr, "pgaccel: %s: sycl::free failed: unknown C++ exception\n", __func__);
  }
}

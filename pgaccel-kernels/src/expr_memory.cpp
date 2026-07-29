// SYCL allocation and copy lifecycle for GPU-resident expression buffers.

#include <sycl/sycl.hpp>

#include <cstdio>
#include <cstring>
#include <exception>

#include "pgaccel_expr.h"
#include "pgaccel_queue.h"

extern "C" pgaccel_status pgaccel_expr_shared_alloc(size_t bytes, void** out) try {
  if (out == nullptr)
    return PGACCEL_ERROR;
  *out = nullptr;
  if (bytes == 0)
    return PGACCEL_OK;

  pgaccel_status init_status = pgaccel_init();
  if (init_status != PGACCEL_OK)
    return init_status;
  sycl::queue* q = pgaccel_get_queue();
  if (q == nullptr)
    return PGACCEL_ERROR_NO_DEVICE;

  void* ptr = nullptr;
  try {
    ptr = sycl::malloc_shared(bytes, *q);
  } catch (const sycl::exception& e) {
    std::fprintf(stderr, "pgaccel: shared allocation failed: %s\n", e.what());
    return PGACCEL_ERROR;
  } catch (const std::exception& e) {
    std::fprintf(stderr, "pgaccel: shared allocation failed: %s\n", e.what());
    return PGACCEL_ERROR;
  }
  if (ptr == nullptr)
    return PGACCEL_OOM;
  *out = ptr;
  return PGACCEL_OK;
} catch (const pgaccel_no_device_error&) {
  return PGACCEL_ERROR_NO_DEVICE;
} catch (const std::exception& e) {
  return pgaccel_kernel_failure("pgaccel_expr_shared_alloc", &e);
} catch (...) {
  return pgaccel_kernel_failure("pgaccel_expr_shared_alloc", nullptr);
}

extern "C" void pgaccel_expr_shared_free(void* ptr) {
  if (ptr == nullptr)
    return;
  if (pgaccel_init() != PGACCEL_OK)
    return;
  sycl::queue* q = pgaccel_get_queue();
  if (q == nullptr)
    return;
  try {
    sycl::free(ptr, *q);
  } catch (const std::exception& e) {
    std::fprintf(stderr, "pgaccel: %s: sycl::free failed: %s\n", __func__, e.what());
  } catch (...) {
    std::fprintf(stderr, "pgaccel: %s: sycl::free failed: unknown C++ exception\n", __func__);
  }
}

extern "C" pgaccel_status pgaccel_expr_device_alloc(size_t bytes, void** out) try {
  if (out == nullptr)
    return PGACCEL_ERROR;
  *out = nullptr;
  if (bytes == 0)
    return PGACCEL_OK;

  pgaccel_status init_status = pgaccel_init();
  if (init_status != PGACCEL_OK)
    return init_status;
  sycl::queue* q = pgaccel_get_queue();
  if (q == nullptr)
    return PGACCEL_ERROR_NO_DEVICE;

  void* ptr = nullptr;
  try {
    ptr = sycl::malloc_device(bytes, *q);
  } catch (const sycl::exception& e) {
    std::fprintf(stderr, "pgaccel: device allocation failed: %s\n", e.what());
    return PGACCEL_ERROR;
  } catch (const std::exception& e) {
    std::fprintf(stderr, "pgaccel: device allocation failed: %s\n", e.what());
    return PGACCEL_ERROR;
  }
  if (ptr == nullptr)
    return PGACCEL_OOM;
  *out = ptr;
  return PGACCEL_OK;
} catch (const pgaccel_no_device_error&) {
  return PGACCEL_ERROR_NO_DEVICE;
} catch (const std::exception& e) {
  return pgaccel_kernel_failure("pgaccel_expr_device_alloc", &e);
} catch (...) {
  return pgaccel_kernel_failure("pgaccel_expr_device_alloc", nullptr);
}

extern "C" pgaccel_status pgaccel_expr_device_alloc_copy(const void* src, size_t bytes,
                                                         void** out) try {
  if (out == nullptr)
    return PGACCEL_ERROR;
  *out = nullptr;
  if (bytes == 0)
    return PGACCEL_OK;
  if (src == nullptr)
    return PGACCEL_ERROR;

#if defined(__APPLE__)
  pgaccel_status init_status = pgaccel_init();
  if (init_status != PGACCEL_OK)
    return init_status;
  sycl::queue* q = pgaccel_get_queue();
  if (q == nullptr)
    return PGACCEL_ERROR_NO_DEVICE;

  void* ptr = nullptr;
  try {
    // On Apple Silicon, shared USM is the stable resident representation for
    // host-built columns. It avoids the post-fork Metal blit path.
    ptr = sycl::malloc_shared(bytes, *q);
    if (ptr == nullptr)
      return PGACCEL_OOM;
    std::memcpy(ptr, src, bytes);
    *out = ptr;
    return PGACCEL_OK;
  } catch (const sycl::exception& e) {
    std::fprintf(stderr, "pgaccel: resident shared copy allocation failed: %s\n", e.what());
  } catch (const std::exception& e) {
    std::fprintf(stderr, "pgaccel: resident shared copy allocation failed: %s\n", e.what());
  }
  if (ptr != nullptr) {
    try {
      sycl::free(ptr, *q);
    } catch (const std::exception& e) {
      std::fprintf(stderr, "pgaccel: resident shared copy cleanup failed: %s\n", e.what());
    } catch (...) {
      std::fprintf(stderr,
                   "pgaccel: resident shared copy cleanup failed (unknown C++ exception)\n");
    }
  }
  return PGACCEL_ERROR;
#else
  void* ptr = nullptr;
  pgaccel_status status = pgaccel_expr_device_alloc(bytes, &ptr);
  if (status != PGACCEL_OK)
    return status;

  sycl::queue* q = pgaccel_get_queue();
  if (q == nullptr)
    return PGACCEL_ERROR_NO_DEVICE;
  try {
    q->memcpy(ptr, src, bytes).wait_and_throw();
  } catch (const sycl::exception& e) {
    std::fprintf(stderr, "pgaccel: device copy failed: %s\n", e.what());
    sycl::free(ptr, *q);
    return PGACCEL_ERROR;
  } catch (const std::exception& e) {
    std::fprintf(stderr, "pgaccel: device copy failed: %s\n", e.what());
    sycl::free(ptr, *q);
    return PGACCEL_ERROR;
  }
  *out = ptr;
  return PGACCEL_OK;
#endif
} catch (const pgaccel_no_device_error&) {
  return PGACCEL_ERROR_NO_DEVICE;
} catch (const std::exception& e) {
  return pgaccel_kernel_failure("pgaccel_expr_device_alloc_copy", &e);
} catch (...) {
  return pgaccel_kernel_failure("pgaccel_expr_device_alloc_copy", nullptr);
}

extern "C" pgaccel_status pgaccel_expr_device_copy_from_host(void* dst, const void* src,
                                                             size_t bytes) try {
  if (bytes == 0)
    return PGACCEL_OK;
  if (dst == nullptr || src == nullptr)
    return PGACCEL_ERROR;
  pgaccel_status init_status = pgaccel_init();
  if (init_status != PGACCEL_OK)
    return init_status;
  sycl::queue* q = pgaccel_get_queue();
  if (q == nullptr)
    return PGACCEL_ERROR_NO_DEVICE;
  try {
#if defined(__APPLE__)
    const sycl::usm::alloc allocation =
        sycl::get_pointer_type(static_cast<const void*>(dst), q->get_context());
    if (allocation == sycl::usm::alloc::shared || allocation == sycl::usm::alloc::host) {
      std::memcpy(dst, src, bytes);
      return PGACCEL_OK;
    }
#endif
    q->memcpy(dst, src, bytes).wait_and_throw();
    return PGACCEL_OK;
  } catch (const sycl::exception& e) {
    std::fprintf(stderr, "pgaccel: device copy from host failed: %s\n", e.what());
    return PGACCEL_ERROR;
  } catch (const std::exception& e) {
    std::fprintf(stderr, "pgaccel: device copy from host failed: %s\n", e.what());
    return PGACCEL_ERROR;
  }
} catch (const pgaccel_no_device_error&) {
  return PGACCEL_ERROR_NO_DEVICE;
} catch (const std::exception& e) {
  return pgaccel_kernel_failure("pgaccel_expr_device_copy_from_host", &e);
} catch (...) {
  return pgaccel_kernel_failure("pgaccel_expr_device_copy_from_host", nullptr);
}

extern "C" pgaccel_status pgaccel_expr_device_copy_to_host(void* dst, const void* src,
                                                           size_t bytes) try {
  if (bytes == 0)
    return PGACCEL_OK;
  if (dst == nullptr || src == nullptr)
    return PGACCEL_ERROR;
  pgaccel_status init_status = pgaccel_init();
  if (init_status != PGACCEL_OK)
    return init_status;
  sycl::queue* q = pgaccel_get_queue();
  if (q == nullptr)
    return PGACCEL_ERROR_NO_DEVICE;
  try {
#if defined(__APPLE__)
    const sycl::usm::alloc allocation = sycl::get_pointer_type(src, q->get_context());
    if (allocation == sycl::usm::alloc::shared || allocation == sycl::usm::alloc::host) {
      std::memcpy(dst, src, bytes);
      return PGACCEL_OK;
    }
#endif
    q->memcpy(dst, src, bytes).wait_and_throw();
    return PGACCEL_OK;
  } catch (const sycl::exception& e) {
    std::fprintf(stderr, "pgaccel: device copy to host failed: %s\n", e.what());
    return PGACCEL_ERROR;
  } catch (const std::exception& e) {
    std::fprintf(stderr, "pgaccel: device copy to host failed: %s\n", e.what());
    return PGACCEL_ERROR;
  }
} catch (const pgaccel_no_device_error&) {
  return PGACCEL_ERROR_NO_DEVICE;
} catch (const std::exception& e) {
  return pgaccel_kernel_failure("pgaccel_expr_device_copy_to_host", &e);
} catch (...) {
  return pgaccel_kernel_failure("pgaccel_expr_device_copy_to_host", nullptr);
}

extern "C" void pgaccel_expr_device_free(void* ptr) {
  if (ptr == nullptr)
    return;
  if (pgaccel_init() != PGACCEL_OK)
    return;
  sycl::queue* q = pgaccel_get_queue();
  if (q == nullptr)
    return;
  try {
    sycl::free(ptr, *q);
  } catch (const std::exception& e) {
    std::fprintf(stderr, "pgaccel: %s: sycl::free failed: %s\n", __func__, e.what());
  } catch (...) {
    std::fprintf(stderr, "pgaccel: %s: sycl::free failed: unknown C++ exception\n", __func__);
  }
}

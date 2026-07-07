/*
 * nested_loop_ineq.cpp — GPU NestedLoop scalar inequality join kernel.
 *
 * Targets the Phase 4 NLJ scalar inequality recognizer in
 * `pg_accel/src/engine/ffi/planner_hooks/join_pathlist.rs` —
 * the planner observability path (`observe_nestloop_scalar_opportunity`)
 * detects correlated scalar btree inequalities (`<`, `<=`, `>=`, `>`,
 * BETWEEN as two conjoined inequalities). This kernel evaluates the
 * predicate for every (outer_i, inner_j) pair via a tiled cross-product
 * scan and emits matching index pairs through an atomic-counter
 * compaction.
 *
 * Layout decisions
 * ----------------
 * - Both keys are flat device buffers (i64 or f64). Caller is responsible
 *   for NULL filtering (PG INNER join semantics: NULL on either side
 *   excludes the row from the output — at the bridge level we strip
 *   nulls before the keys reach the kernel).
 * - Predicate is a single opcode (lt/le/ge/gt) — for the BETWEEN /
 *   interval-overlap shape we expose a dedicated entry point
 *   `pgaccel_nlj_between_i64` / `_f64` that evaluates
 *   `inner_lo[j] <= outer[i] <= inner_hi[j]` in one pass, mirroring
 *   what PG's planner does when it expands `A.ts BETWEEN B.lo AND B.hi`
 *   into two btree quals.
 * - Output is a flat `[outer_idx, inner_idx, outer_idx, inner_idx, ...]`
 *   u32 buffer. `max_pairs` is enforced — if the kernel overflows
 *   (`*pair_count > max_pairs`), the caller MUST reject the result and
 *   fall back to PG native NLJ (no silent truncation).
 *
 * Per-pair work is one comparison; total work is `N * M`. The kernel
 * launches one work-item per pair so the work distribution is uniform.
 *
 * The pair-compaction atomic uses 64-bit when the device supports
 * `sycl::aspect::atomic64`, else 32-bit (matching bbox_ops.cpp).
 */

#include <sycl/sycl.hpp>

#include <cstddef>
#include <cstdint>
#include <stdexcept>

#include "pgaccel_ffi.h"
#include "pgaccel_queue.h"
#include "pgaccel_nested_loop_ineq.h"

// SAFETY: g_queue is owned by device_manager.cpp.

namespace {

static sycl::queue& get_queue() {
  return pgaccel_require_queue();
}

// ---------------------------------------------------------------------------
// Predicate evaluation
// ---------------------------------------------------------------------------

template <typename T>
static inline bool eval_ineq(T a, T b, pgaccel_nlj_ineq_op op) {
  switch (op) {
    case PGACCEL_NLJ_LT:
      return a < b;
    case PGACCEL_NLJ_LE:
      return a <= b;
    case PGACCEL_NLJ_GE:
      return a >= b;
    case PGACCEL_NLJ_GT:
      return a > b;
  }
  // Unreachable; defensive.
  return false;
}

// ---------------------------------------------------------------------------
// Pair-emit kernel implementation (parametrised on element type T and atomic
// counter type C). Predicate is `eval_ineq(outer[i], inner[j], op)`.
//
// max_pairs is enforced by the kernel: each work-item that matches first
// reserves a slot via `fetch_add`. If the reservation index is >= max_pairs
// we discard the write but still increment the counter so the caller can
// detect overflow by comparing `*pair_count > max_pairs`.
// ---------------------------------------------------------------------------

template <typename T, typename C>
pgaccel_status nlj_ineq_impl(sycl::queue& q, const T* outer_keys, size_t n_outer,
                             const T* inner_keys, size_t n_inner, pgaccel_nlj_ineq_op op,
                             uint32_t* pairs_out, size_t max_pairs, size_t* pair_count_out) {
  if (n_outer == 0 || n_inner == 0) {
    if (pair_count_out)
      *pair_count_out = 0;
    return PGACCEL_OK;
  }
  if (!outer_keys || !inner_keys || !pairs_out || !pair_count_out) {
    return PGACCEL_ERROR;
  }
  // Defensive overflow check on i*j.
  if (n_outer > UINT32_MAX || n_inner > UINT32_MAX) {
    return PGACCEL_ERROR;
  }

  const size_t total_pairs = n_outer * n_inner;

  T* d_outer = sycl::malloc_device<T>(n_outer, q);
  T* d_inner = sycl::malloc_device<T>(n_inner, q);
  // pairs buffer: 2 u32 per slot.
  uint32_t* d_pairs = sycl::malloc_device<uint32_t>(max_pairs * 2 + 2, q);
  C* d_count = sycl::malloc_device<C>(1, q);

  if (!d_outer || !d_inner || !d_pairs || !d_count) {
    sycl::free(d_outer, q);
    sycl::free(d_inner, q);
    sycl::free(d_pairs, q);
    sycl::free(d_count, q);
    return PGACCEL_OOM;
  }

  try {
    q.memcpy(d_outer, outer_keys, n_outer * sizeof(T));
    q.memcpy(d_inner, inner_keys, n_inner * sizeof(T));
    q.memset(d_count, 0, sizeof(C));
    q.wait_and_throw();

    const size_t cb = n_inner;
    const C cap = static_cast<C>(max_pairs);

    q.submit([&](sycl::handler& h) {
       h.parallel_for(sycl::range<1>(total_pairs), [=](sycl::id<1> id) {
         const size_t idx = id[0];
         const size_t i = idx / cb;
         const size_t j = idx % cb;

         const T a = d_outer[i];
         const T b = d_inner[j];
         const bool matched = eval_ineq<T>(a, b, op);
         if (!matched) {
           return;
         }
         sycl::atomic_ref<C, sycl::memory_order::relaxed, sycl::memory_scope::device,
                          sycl::access::address_space::global_space>
             cnt(*d_count);
         const C slot = cnt.fetch_add(static_cast<C>(1));
         if (slot < cap) {
           d_pairs[slot * 2 + 0] = static_cast<uint32_t>(i);
           d_pairs[slot * 2 + 1] = static_cast<uint32_t>(j);
         }
         // Else: overflow — caller compares *pair_count vs max_pairs.
       });
     }).wait_and_throw();

    C gpu_count = 0;
    q.memcpy(&gpu_count, d_count, sizeof(C));
    q.wait_and_throw();

    const size_t emitted =
        (static_cast<size_t>(gpu_count) <= max_pairs) ? static_cast<size_t>(gpu_count) : max_pairs;
    if (emitted > 0) {
      q.memcpy(pairs_out, d_pairs, emitted * 2 * sizeof(uint32_t));
      q.wait_and_throw();
    }
    *pair_count_out = static_cast<size_t>(gpu_count);
  } catch (...) {
    sycl::free(d_outer, q);
    sycl::free(d_inner, q);
    sycl::free(d_pairs, q);
    sycl::free(d_count, q);
    throw;
  }

  sycl::free(d_outer, q);
  sycl::free(d_inner, q);
  sycl::free(d_pairs, q);
  sycl::free(d_count, q);

  return PGACCEL_OK;
}

template <typename T>
pgaccel_status nlj_ineq_dispatch(sycl::queue& q, const T* outer_keys, size_t n_outer,
                                 const T* inner_keys, size_t n_inner, pgaccel_nlj_ineq_op op,
                                 uint32_t* pairs_out, size_t max_pairs, size_t* pair_count_out) {
  const pgaccel_platform_caps caps = pgaccel_get_caps();
  if (caps.has_atomic64) {
    return nlj_ineq_impl<T, uint64_t>(q, outer_keys, n_outer, inner_keys, n_inner, op, pairs_out,
                                      max_pairs, pair_count_out);
  }
  return nlj_ineq_impl<T, uint32_t>(q, outer_keys, n_outer, inner_keys, n_inner, op, pairs_out,
                                    max_pairs, pair_count_out);
}

// ---------------------------------------------------------------------------
// BETWEEN-shape implementation: predicate is
//   inner_lo[j] <= outer[i] <= inner_hi[j]
// Caller is responsible for stripping NULLs from any of the three arrays
// before calling.
// ---------------------------------------------------------------------------

template <typename T, typename C>
pgaccel_status nlj_between_impl(sycl::queue& q, const T* outer_keys, size_t n_outer,
                                const T* inner_lo, const T* inner_hi, size_t n_inner,
                                uint32_t* pairs_out, size_t max_pairs, size_t* pair_count_out) {
  if (n_outer == 0 || n_inner == 0) {
    if (pair_count_out)
      *pair_count_out = 0;
    return PGACCEL_OK;
  }
  if (!outer_keys || !inner_lo || !inner_hi || !pairs_out || !pair_count_out) {
    return PGACCEL_ERROR;
  }
  if (n_outer > UINT32_MAX || n_inner > UINT32_MAX) {
    return PGACCEL_ERROR;
  }

  const size_t total_pairs = n_outer * n_inner;

  T* d_outer = sycl::malloc_device<T>(n_outer, q);
  T* d_lo = sycl::malloc_device<T>(n_inner, q);
  T* d_hi = sycl::malloc_device<T>(n_inner, q);
  uint32_t* d_pairs = sycl::malloc_device<uint32_t>(max_pairs * 2 + 2, q);
  C* d_count = sycl::malloc_device<C>(1, q);

  if (!d_outer || !d_lo || !d_hi || !d_pairs || !d_count) {
    sycl::free(d_outer, q);
    sycl::free(d_lo, q);
    sycl::free(d_hi, q);
    sycl::free(d_pairs, q);
    sycl::free(d_count, q);
    return PGACCEL_OOM;
  }

  try {
    q.memcpy(d_outer, outer_keys, n_outer * sizeof(T));
    q.memcpy(d_lo, inner_lo, n_inner * sizeof(T));
    q.memcpy(d_hi, inner_hi, n_inner * sizeof(T));
    q.memset(d_count, 0, sizeof(C));
    q.wait_and_throw();

    const size_t cb = n_inner;
    const C cap = static_cast<C>(max_pairs);

    q.submit([&](sycl::handler& h) {
       h.parallel_for(sycl::range<1>(total_pairs), [=](sycl::id<1> id) {
         const size_t idx = id[0];
         const size_t i = idx / cb;
         const size_t j = idx % cb;

         const T x = d_outer[i];
         const T lo = d_lo[j];
         const T hi = d_hi[j];
         const bool matched = (x >= lo) && (x <= hi);
         if (!matched) {
           return;
         }
         sycl::atomic_ref<C, sycl::memory_order::relaxed, sycl::memory_scope::device,
                          sycl::access::address_space::global_space>
             cnt(*d_count);
         const C slot = cnt.fetch_add(static_cast<C>(1));
         if (slot < cap) {
           d_pairs[slot * 2 + 0] = static_cast<uint32_t>(i);
           d_pairs[slot * 2 + 1] = static_cast<uint32_t>(j);
         }
       });
     }).wait_and_throw();

    C gpu_count = 0;
    q.memcpy(&gpu_count, d_count, sizeof(C));
    q.wait_and_throw();

    const size_t emitted =
        (static_cast<size_t>(gpu_count) <= max_pairs) ? static_cast<size_t>(gpu_count) : max_pairs;
    if (emitted > 0) {
      q.memcpy(pairs_out, d_pairs, emitted * 2 * sizeof(uint32_t));
      q.wait_and_throw();
    }
    *pair_count_out = static_cast<size_t>(gpu_count);
  } catch (...) {
    sycl::free(d_outer, q);
    sycl::free(d_lo, q);
    sycl::free(d_hi, q);
    sycl::free(d_pairs, q);
    sycl::free(d_count, q);
    throw;
  }

  sycl::free(d_outer, q);
  sycl::free(d_lo, q);
  sycl::free(d_hi, q);
  sycl::free(d_pairs, q);
  sycl::free(d_count, q);

  return PGACCEL_OK;
}

template <typename T>
pgaccel_status nlj_between_dispatch(sycl::queue& q, const T* outer_keys, size_t n_outer,
                                    const T* inner_lo, const T* inner_hi, size_t n_inner,
                                    uint32_t* pairs_out, size_t max_pairs, size_t* pair_count_out) {
  const pgaccel_platform_caps caps = pgaccel_get_caps();
  if (caps.has_atomic64) {
    return nlj_between_impl<T, uint64_t>(q, outer_keys, n_outer, inner_lo, inner_hi, n_inner,
                                         pairs_out, max_pairs, pair_count_out);
  }
  return nlj_between_impl<T, uint32_t>(q, outer_keys, n_outer, inner_lo, inner_hi, n_inner,
                                       pairs_out, max_pairs, pair_count_out);
}

}  // anonymous namespace

// ---------------------------------------------------------------------------
// Public C API — declared in pgaccel_nested_loop_ineq.h.
// ---------------------------------------------------------------------------

extern "C" pgaccel_status pgaccel_nlj_ineq_i64(const int64_t* outer_keys, size_t n_outer,
                                               const int64_t* inner_keys, size_t n_inner,
                                               pgaccel_nlj_ineq_op op, uint32_t* pairs_out,
                                               size_t max_pairs, size_t* pair_count_out) try {
  try {
    sycl::queue& q = get_queue();
    pgaccel_status st = nlj_ineq_dispatch<int64_t>(q, outer_keys, n_outer, inner_keys, n_inner, op,
                                                   pairs_out, max_pairs, pair_count_out);
    if (st == PGACCEL_OK)
      pgaccel_record_gpu_exec();
    return st;
    return st;
  } catch (const pgaccel_no_device_error&) {
    return PGACCEL_ERROR_NO_DEVICE;
  } catch (const std::exception& e) {
    return pgaccel_kernel_failure(__func__, &e);
  } catch (...) {
    return pgaccel_kernel_failure(__func__, nullptr);
  }
  return PGACCEL_ERROR_NO_DEVICE;
} catch (const pgaccel_no_device_error&) {
  return PGACCEL_ERROR_NO_DEVICE;
} catch (const std::exception& e) {
  return pgaccel_kernel_failure("pgaccel_nlj_ineq_i64", &e);
} catch (...) {
  return pgaccel_kernel_failure("pgaccel_nlj_ineq_i64", nullptr);
}

extern "C" pgaccel_status pgaccel_nlj_ineq_f64(const double* outer_keys, size_t n_outer,
                                               const double* inner_keys, size_t n_inner,
                                               pgaccel_nlj_ineq_op op, uint32_t* pairs_out,
                                               size_t max_pairs, size_t* pair_count_out) try {
  try {
    sycl::queue& q = get_queue();
    pgaccel_status st = nlj_ineq_dispatch<double>(q, outer_keys, n_outer, inner_keys, n_inner, op,
                                                  pairs_out, max_pairs, pair_count_out);
    if (st == PGACCEL_OK)
      pgaccel_record_gpu_exec();
    return st;
    return st;
  } catch (const pgaccel_no_device_error&) {
    return PGACCEL_ERROR_NO_DEVICE;
  } catch (const std::exception& e) {
    return pgaccel_kernel_failure(__func__, &e);
  } catch (...) {
    return pgaccel_kernel_failure(__func__, nullptr);
  }
  return PGACCEL_ERROR_NO_DEVICE;
} catch (const pgaccel_no_device_error&) {
  return PGACCEL_ERROR_NO_DEVICE;
} catch (const std::exception& e) {
  return pgaccel_kernel_failure("pgaccel_nlj_ineq_f64", &e);
} catch (...) {
  return pgaccel_kernel_failure("pgaccel_nlj_ineq_f64", nullptr);
}

extern "C" pgaccel_status pgaccel_nlj_between_i64(const int64_t* outer_keys, size_t n_outer,
                                                  const int64_t* inner_lo, const int64_t* inner_hi,
                                                  size_t n_inner, uint32_t* pairs_out,
                                                  size_t max_pairs, size_t* pair_count_out) try {
  try {
    sycl::queue& q = get_queue();
    pgaccel_status st = nlj_between_dispatch<int64_t>(
        q, outer_keys, n_outer, inner_lo, inner_hi, n_inner, pairs_out, max_pairs, pair_count_out);
    if (st == PGACCEL_OK)
      pgaccel_record_gpu_exec();
    return st;
    return st;
  } catch (const pgaccel_no_device_error&) {
    return PGACCEL_ERROR_NO_DEVICE;
  } catch (const std::exception& e) {
    return pgaccel_kernel_failure(__func__, &e);
  } catch (...) {
    return pgaccel_kernel_failure(__func__, nullptr);
  }
  return PGACCEL_ERROR_NO_DEVICE;
} catch (const pgaccel_no_device_error&) {
  return PGACCEL_ERROR_NO_DEVICE;
} catch (const std::exception& e) {
  return pgaccel_kernel_failure("pgaccel_nlj_between_i64", &e);
} catch (...) {
  return pgaccel_kernel_failure("pgaccel_nlj_between_i64", nullptr);
}

extern "C" pgaccel_status pgaccel_nlj_between_f64(const double* outer_keys, size_t n_outer,
                                                  const double* inner_lo, const double* inner_hi,
                                                  size_t n_inner, uint32_t* pairs_out,
                                                  size_t max_pairs, size_t* pair_count_out) try {
  try {
    sycl::queue& q = get_queue();
    pgaccel_status st = nlj_between_dispatch<double>(q, outer_keys, n_outer, inner_lo, inner_hi,
                                                     n_inner, pairs_out, max_pairs, pair_count_out);
    if (st == PGACCEL_OK)
      pgaccel_record_gpu_exec();
    return st;
    return st;
  } catch (const pgaccel_no_device_error&) {
    return PGACCEL_ERROR_NO_DEVICE;
  } catch (const std::exception& e) {
    return pgaccel_kernel_failure(__func__, &e);
  } catch (...) {
    return pgaccel_kernel_failure(__func__, nullptr);
  }
  return PGACCEL_ERROR_NO_DEVICE;
} catch (const pgaccel_no_device_error&) {
  return PGACCEL_ERROR_NO_DEVICE;
} catch (const std::exception& e) {
  return pgaccel_kernel_failure("pgaccel_nlj_between_f64", &e);
} catch (...) {
  return pgaccel_kernel_failure("pgaccel_nlj_between_f64", nullptr);
}

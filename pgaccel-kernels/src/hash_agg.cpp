/*
 * hash_agg.cpp — GPU hash aggregation: grouped SUM/MIN/MAX/COUNT.
 *
 * Group assignment is performed only by GPU-resident paths:
 *   - `agg_hash_row_parallel`: device hash-table group assignment.
 *   - `agg_sort_based`: GPU sort by group key, followed by grouped reduce.
 *   - `agg_hash_streaming_numeric`: bounded staging into a persistent device
 *     hash table for the standalone checked ABI.
 *
 * All accumulators use f64 internally to prevent integer overflow
 * (int32 SUM can overflow int32 after ~2B rows; f64 gives ~15 digits
 * of precision which is sufficient for partial aggregates).
 *
 * NULL group keys are accumulated into a single "NULL group" (like PG).
 * NULL values are skipped for SUM/MIN/MAX but not for COUNT(*).
 *
 * Unsupported shapes are declined before queue creation. There is no host
 * hash-table fallback behind the public pgaccel hash-aggregate APIs.
 */

#include <sycl/sycl.hpp>

#include <algorithm>
#include <cmath>
#include <cstdint>
#include <cstdio>
#include <cstdlib>
#include <cstring>
#include <exception>
#include <limits>
#include <new>
#include <vector>

#include "pgaccel_ffi.h"
#include "pgaccel_hash_agg.h"
#include "pgaccel_queue.h"

// ---------------------------------------------------------------------------
// Hash functions (same as hash_join.cpp)
// ---------------------------------------------------------------------------

static inline uint64_t hash64(uint64_t k) {
  k ^= k >> 33;
  k *= 0xff51afd7ed558ccdULL;
  k ^= k >> 33;
  k *= 0xc4ceb9fe1a85ec53ULL;
  k ^= k >> 33;
  return k;
}

// ---------------------------------------------------------------------------
// Aggregation state
// ---------------------------------------------------------------------------

struct pgaccel_agg_state {
  /// Group key bytes (type-erased, contiguous).
  std::vector<uint8_t> group_key_buf;
  size_t key_size;
  int key_type;
  size_t group_count;
  size_t num_aggs;

  /// Per-group counts.
  std::vector<int64_t> counts;

  /// Per-aggregate result arrays (flattened: one Vec<double> per agg, length = group_count).
  /// Finalize-mode only — width=1 per group.
  std::vector<std::vector<double>> results;

  /// Per-aggregate **partial-mode** result arrays. Each inner vector has
  /// length `group_count * partial_widths[a]`, laid out group-major:
  /// `[g0_lane0, g0_lane1, ..., g1_lane0, ...]`. Empty when this state
  /// was produced by the finalize-mode kernel.
  std::vector<std::vector<double>> partial_results;
  /// Per-aggregate lane widths (1 for SUM/MIN/MAX/COUNT, 2 for AVG, 3 for
  /// STDDEV/VAR). Length = num_aggs. Empty when state is finalize-mode.
  std::vector<size_t> partial_widths;
};

// ---------------------------------------------------------------------------
// Read a value from a typed column
// ---------------------------------------------------------------------------

namespace {

// Device-callable typed read result. The actual read function
// (`device_read_value_flat`) lives in the second `namespace { ... }` block
// below; it operates on a single flat shared buffer with per-agg byte
// offsets, sidestepping the `device device void**` argbuffer access
// pattern that an array of typed pointers would emit on Metal SSCP.
struct val_read {
  double value;
  bool is_null;
};

}  // namespace

static inline size_t key_size_for_type(int key_type) {
  switch (key_type) {
    case 0:
      return sizeof(int32_t);
    case 1:
      return sizeof(int64_t);
    case 2:
      return sizeof(double);
    case 4:
      return 16; /* UUID */
    case 5:
      return 24; /* INET / CIDR canonical key (see pgaccel_hash_join.h) */
    default:
      return 0;
  }
}

static inline bool null_sort_sentinel_collides(const void* group_keys,
                                               const uint8_t* group_null_mask, size_t row_count,
                                               int key_type) {
  if (group_null_mask == nullptr)
    return false;

  bool has_null = false;
  for (size_t i = 0; i < row_count; ++i) {
    if (group_null_mask[i]) {
      has_null = true;
      break;
    }
  }
  if (!has_null)
    return false;

  switch (key_type) {
    case 0: {
      const auto* src = static_cast<const int32_t*>(group_keys);
      for (size_t i = 0; i < row_count; ++i) {
        if (!group_null_mask[i] && src[i] == std::numeric_limits<int32_t>::max())
          return true;
      }
      return false;
    }
    case 1: {
      const auto* src = static_cast<const int64_t*>(group_keys);
      for (size_t i = 0; i < row_count; ++i) {
        if (!group_null_mask[i] && src[i] == std::numeric_limits<int64_t>::max())
          return true;
      }
      return false;
    }
    case 2: {
      const auto* src = static_cast<const double*>(group_keys);
      for (size_t i = 0; i < row_count; ++i) {
        if (!group_null_mask[i] && std::isinf(src[i]) && src[i] > 0.0)
          return true;
      }
      return false;
    }
    default:
      return false;
  }
}

// ---------------------------------------------------------------------------
// SYCL accumulation kernel: dispatches one work-item per (group, agg).
//
// Uses sequential per-group scan inside the work-item — the row->group
// mapping in `row_to_group` lets each work-item find its rows by linear
// scan. For sorted input (`agg_sort_based` path), the per-group rows
// are contiguous and supplied via `group_starts`/`group_ends` so the
// kernel does NOT scan all rows for each group. For unsorted input
// (`agg_hash` path) the kernel scans all rows once per group; this is
// O(n*g), so large-row fallback is admitted only for low-cardinality
// batches.
// ---------------------------------------------------------------------------

namespace {

// Device-callable typed value read that operates on a SINGLE flat shared
// buffer carrying all per-agg column data concatenated end-to-end. The
// per-agg starting byte offset into the flat buffer is in
// `value_offsets[a]`; rows within an agg's slab live at `byte_offset =
// value_offsets[a] + row * elem_size_for(val_type)`.
//
// Why a flat buffer (vs an array of typed pointers): on Metal the
// AdaptiveCpp SSCP backend cannot capture an array of device pointers
// (`const void* const*`) — the resulting argbuffer access pattern emits
// `device device void**` which fails MSL validation
// ("C-style cast from 'device void *' to 'device void **' converts between
// mismatching address spaces", see commit 6de58d6). Capturing a single
// `const uint8_t*` (1 slot in the argbuffer) and computing offsets
// inside the kernel avoids the pointer-of-pointer pattern entirely while
// preserving correct device-address-space tagging on the typed reads.
//
// Per-element width is recovered from `val_type` (must match the type
// used at staging time so offsets line up).
inline size_t value_elem_size(int val_type) {
  switch (val_type) {
    case 1:
      return sizeof(bool);
    case 2:
      return sizeof(int32_t);
    case 3:
      return sizeof(int64_t);
    case 4:
      return sizeof(float);
    case 5:
      return sizeof(double);
    default:
      return 0;
  }
}

inline bool checked_add_size(size_t a, size_t b, size_t* out) {
  if (out == nullptr)
    return false;
  if (a > std::numeric_limits<size_t>::max() - b)
    return false;
  *out = a + b;
  return true;
}

inline bool checked_mul_size(size_t a, size_t b, size_t* out) {
  if (out == nullptr)
    return false;
  if (a != 0 && b > std::numeric_limits<size_t>::max() / a)
    return false;
  *out = a * b;
  return true;
}

inline bool checked_align_up(size_t value, size_t alignment, size_t* out) {
  if (out == nullptr || alignment == 0)
    return false;
  const size_t rem = value % alignment;
  if (rem == 0) {
    *out = value;
    return true;
  }
  return checked_add_size(value, alignment - rem, out);
}

inline bool is_valid_agg_func(pgaccel_agg_func func) {
  switch (func) {
    case PGACCEL_AGG_SUM:
    case PGACCEL_AGG_MIN:
    case PGACCEL_AGG_MAX:
    case PGACCEL_AGG_COUNT:
    case PGACCEL_AGG_AVG:
    case PGACCEL_AGG_STDDEV:
    case PGACCEL_AGG_VAR:
      return true;
    default:
      return false;
  }
}

inline bool is_partial_only_agg_func(pgaccel_agg_func func) {
  return func == PGACCEL_AGG_AVG || func == PGACCEL_AGG_STDDEV || func == PGACCEL_AGG_VAR;
}

inline bool agg_reads_value(pgaccel_agg_func func, size_t col_idx) {
  return !(func == PGACCEL_AGG_COUNT && col_idx == SIZE_MAX);
}

bool hashagg_sort_based_available() {
  const pgaccel_platform_caps caps = pgaccel_get_caps();
  return std::strcmp(caps.backend_name, "metal") != 0;
}

bool validate_hashagg_inputs(const void* group_keys, size_t row_count, int key_type,
                             const void* const* value_cols, const int* value_types,
                             const pgaccel_agg_col* agg_cols, size_t num_aggs, bool partial_mode) {
  if (row_count == 0 || group_keys == nullptr || agg_cols == nullptr || value_types == nullptr ||
      num_aggs == 0)
    return false;
  if (key_size_for_type(key_type) == 0)
    return false;

  for (size_t a = 0; a < num_aggs; ++a) {
    const pgaccel_agg_func func = agg_cols[a].func;
    if (!is_valid_agg_func(func))
      return false;
    if (!partial_mode && is_partial_only_agg_func(func))
      return false;
    if (value_elem_size(value_types[a]) == 0)
      return false;
    if (agg_reads_value(func, agg_cols[a].col_idx) &&
        (value_cols == nullptr || value_cols[a] == nullptr))
      return false;
  }
  return true;
}

inline val_read device_read_value_flat(const uint8_t* value_data, const uint8_t* null_data,
                                       size_t value_offset, size_t null_offset, bool null_present,
                                       size_t row, int val_type) {
  val_read r = {0.0, true};
  if (null_present) {
    if (null_data[null_offset + row])
      return r;
  }
  r.is_null = false;
  const uint8_t* p = value_data + value_offset;
  switch (val_type) {
    case 1:  // BOOL
      r.value = reinterpret_cast<const bool*>(p)[row] ? 1.0 : 0.0;
      break;
    case 2:  // INT32
      r.value = static_cast<double>(reinterpret_cast<const int32_t*>(p)[row]);
      break;
    case 3:  // INT64
      r.value = static_cast<double>(reinterpret_cast<const int64_t*>(p)[row]);
      break;
    case 4:  // FLOAT32
      r.value = static_cast<double>(reinterpret_cast<const float*>(p)[row]);
      break;
    case 5:  // FLOAT64
      r.value = reinterpret_cast<const double*>(p)[row];
      break;
    default:
      r.is_null = true;
      break;
  }
  return r;
}

// Metal SSCP can pack lambdas with many captures into an argument-buffer
// reflection path that is not fork-safe. HashAgg kernels instead capture
// one shared `uint8_t*` slab and recover every input/output by byte offset.
struct HashAggKernelSlabHeader {
  size_t n_groups;
  size_t row_count;
  size_t num_aggs;
  size_t indices_off;
  size_t group_starts_off;
  size_t group_ends_off;
  size_t row_to_group_off;
  size_t value_data_off;
  size_t null_data_off;
  size_t value_offsets_off;
  size_t null_offsets_off;
  size_t null_present_off;
  size_t value_types_off;
  size_t agg_funcs_off;
  size_t agg_col_idx_off;
  size_t agg_offsets_off;
  size_t agg_widths_off;
  size_t out_results_off;
  size_t out_counts_off;
};

uint8_t* make_hashagg_kernel_slab(sycl::queue& q, size_t n_groups, size_t row_count,
                                  size_t num_aggs, const uint32_t* indices,
                                  const size_t* group_starts, const size_t* group_ends,
                                  const size_t* row_to_group, const uint8_t* value_data,
                                  size_t value_data_bytes, const uint8_t* null_data,
                                  size_t null_data_bytes, const size_t* value_offsets,
                                  const size_t* null_offsets, const uint8_t* null_present,
                                  const int* value_types, const pgaccel_agg_func* agg_funcs,
                                  const size_t* agg_col_idx, const size_t* agg_offsets,
                                  const size_t* agg_widths, size_t output_double_count);

// Run the accumulation kernel for a sorted-input path.
// `indices[i]` gives the original row index for sort position `i`.
// `group_starts[g]` / `group_ends[g]` bracket the sorted positions
// belonging to group `g`. Each work-item handles one group.
//
// `out_results` is a flat (num_aggs * n_groups) f64 buffer.
// `out_counts` is a length-n_groups i64 buffer.
//
// All input buffers must already live in shared memory.
bool run_sorted_accum_kernel(sycl::queue& q, size_t n_groups, size_t num_aggs,
                             const uint32_t* indices, const size_t* group_starts,
                             const size_t* group_ends, const uint8_t* value_data,
                             size_t value_data_bytes, const uint8_t* null_data,
                             size_t null_data_bytes, const size_t* value_offsets,
                             const size_t* null_offsets, const uint8_t* null_present,
                             const int* value_types, const pgaccel_agg_func* agg_funcs,
                             const size_t* agg_col_idx, double* out_results, int64_t* out_counts) {
  const size_t row_count = n_groups > 0 ? group_ends[n_groups - 1] : 0;
  size_t output_double_count = 0;
  size_t output_bytes = 0;
  size_t counts_bytes = 0;
  if (!checked_mul_size(num_aggs, n_groups, &output_double_count) ||
      !checked_mul_size(output_double_count, sizeof(double), &output_bytes) ||
      !checked_mul_size(n_groups, sizeof(int64_t), &counts_bytes))
    return false;
  uint8_t* slab = make_hashagg_kernel_slab(
      q, n_groups, row_count, num_aggs, indices, group_starts, group_ends, nullptr, value_data,
      value_data_bytes, null_data, null_data_bytes, value_offsets, null_offsets, null_present,
      value_types, agg_funcs, agg_col_idx, nullptr, nullptr, output_double_count);
  if (slab == nullptr)
    return false;
  try {
    q.parallel_for(sycl::range<1>(n_groups), [=](sycl::id<1> id) {
       const auto* h = reinterpret_cast<const HashAggKernelSlabHeader*>(slab);
       const auto* local_indices = reinterpret_cast<const uint32_t*>(slab + h->indices_off);
       const auto* local_group_starts = reinterpret_cast<const size_t*>(slab + h->group_starts_off);
       const auto* local_group_ends = reinterpret_cast<const size_t*>(slab + h->group_ends_off);
       const uint8_t* local_value_data = slab + h->value_data_off;
       const uint8_t* local_null_data = slab + h->null_data_off;
       const auto* local_value_offsets =
           reinterpret_cast<const size_t*>(slab + h->value_offsets_off);
       const auto* local_null_offsets = reinterpret_cast<const size_t*>(slab + h->null_offsets_off);
       const uint8_t* local_null_present = slab + h->null_present_off;
       const auto* local_value_types = reinterpret_cast<const int*>(slab + h->value_types_off);
       const auto* local_agg_funcs =
           reinterpret_cast<const pgaccel_agg_func*>(slab + h->agg_funcs_off);
       const auto* local_agg_col_idx = reinterpret_cast<const size_t*>(slab + h->agg_col_idx_off);
       auto* local_out_results = reinterpret_cast<double*>(slab + h->out_results_off);
       auto* local_out_counts = reinterpret_cast<int64_t*>(slab + h->out_counts_off);
       const size_t g = id[0];
       const size_t start = local_group_starts[g];
       const size_t end = local_group_ends[g];

       int64_t cnt = 0;

       for (size_t a = 0; a < h->num_aggs; ++a) {
         const pgaccel_agg_func func = local_agg_funcs[a];
         const size_t col = local_agg_col_idx[a];
         const size_t voff = local_value_offsets[a];
         const size_t noff = local_null_offsets[a];
         const bool nullable = local_null_present[a] != 0;
         const int vtype = local_value_types[a];

         double acc = 0.0;
         if (func == PGACCEL_AGG_MIN)
           acc = std::numeric_limits<double>::infinity();
         else if (func == PGACCEL_AGG_MAX)
           acc = -std::numeric_limits<double>::infinity();

         for (size_t i = start; i < end; ++i) {
           const uint32_t r = local_indices[i];
           // Bump count once per row (only on the first agg's pass).
           if (a == 0)
             ++cnt;

           if (func == PGACCEL_AGG_COUNT && col == SIZE_MAX) {
             acc += 1.0;
             continue;
           }

           val_read vr = device_read_value_flat(local_value_data, local_null_data, voff, noff,
                                                nullable, r, vtype);
           if (vr.is_null)
             continue;

           switch (func) {
             case PGACCEL_AGG_SUM:
               acc += vr.value;
               break;
             case PGACCEL_AGG_MIN:
               if (vr.value < acc)
                 acc = vr.value;
               break;
             case PGACCEL_AGG_MAX:
               if (vr.value > acc)
                 acc = vr.value;
               break;
             case PGACCEL_AGG_COUNT:
               acc += 1.0;
               break;
             case PGACCEL_AGG_AVG:
             case PGACCEL_AGG_STDDEV:
             case PGACCEL_AGG_VAR:
               // Partial-mode-only functions are rejected by
               // pgaccel_hash_agg_execute before this kernel launches.
               break;
           }
         }
         local_out_results[a * h->n_groups + g] = acc;
       }
       local_out_counts[g] = cnt;
     }).wait_and_throw();
  } catch (const std::exception& e) {
    std::fprintf(stderr, "pgaccel: hash_agg sorted kernel failed: %s\n", e.what());
    sycl::free(slab, q);
    return false;
  } catch (...) {
    std::fprintf(stderr, "pgaccel: hash_agg sorted kernel failed (unknown)\n");
    sycl::free(slab, q);
    return false;
  }
  const auto* h = reinterpret_cast<const HashAggKernelSlabHeader*>(slab);
  std::memcpy(out_results, slab + h->out_results_off, output_bytes);
  std::memcpy(out_counts, slab + h->out_counts_off, counts_bytes);
  sycl::free(slab, q);
  return true;
}

// Run the accumulation kernel for an unsorted-input path.
// `row_to_group[r]` gives the group index for row `r` (or `SIZE_MAX` if
// the row was not assigned). One work-item per group; each scans all
// rows linearly to find the ones belonging to its group. Acceptable
// when n_groups is small; large-row fallback is capped before launch.
bool run_unsorted_accum_kernel(sycl::queue& q, size_t n_groups, size_t row_count, size_t num_aggs,
                               const size_t* row_to_group, const uint8_t* value_data,
                               size_t value_data_bytes, const uint8_t* null_data,
                               size_t null_data_bytes, const size_t* value_offsets,
                               const size_t* null_offsets, const uint8_t* null_present,
                               const int* value_types, const pgaccel_agg_func* agg_funcs,
                               const size_t* agg_col_idx, double* out_results,
                               int64_t* out_counts) {
  size_t output_double_count = 0;
  size_t output_bytes = 0;
  size_t counts_bytes = 0;
  if (!checked_mul_size(num_aggs, n_groups, &output_double_count) ||
      !checked_mul_size(output_double_count, sizeof(double), &output_bytes) ||
      !checked_mul_size(n_groups, sizeof(int64_t), &counts_bytes))
    return false;
  uint8_t* slab = make_hashagg_kernel_slab(
      q, n_groups, row_count, num_aggs, nullptr, nullptr, nullptr, row_to_group, value_data,
      value_data_bytes, null_data, null_data_bytes, value_offsets, null_offsets, null_present,
      value_types, agg_funcs, agg_col_idx, nullptr, nullptr, output_double_count);
  if (slab == nullptr)
    return false;
  try {
    q.parallel_for(sycl::range<1>(n_groups), [=](sycl::id<1> id) {
       const auto* h = reinterpret_cast<const HashAggKernelSlabHeader*>(slab);
       const auto* local_row_to_group = reinterpret_cast<const size_t*>(slab + h->row_to_group_off);
       const uint8_t* local_value_data = slab + h->value_data_off;
       const uint8_t* local_null_data = slab + h->null_data_off;
       const auto* local_value_offsets =
           reinterpret_cast<const size_t*>(slab + h->value_offsets_off);
       const auto* local_null_offsets = reinterpret_cast<const size_t*>(slab + h->null_offsets_off);
       const uint8_t* local_null_present = slab + h->null_present_off;
       const auto* local_value_types = reinterpret_cast<const int*>(slab + h->value_types_off);
       const auto* local_agg_funcs =
           reinterpret_cast<const pgaccel_agg_func*>(slab + h->agg_funcs_off);
       const auto* local_agg_col_idx = reinterpret_cast<const size_t*>(slab + h->agg_col_idx_off);
       auto* local_out_results = reinterpret_cast<double*>(slab + h->out_results_off);
       auto* local_out_counts = reinterpret_cast<int64_t*>(slab + h->out_counts_off);
       const size_t g = id[0];

       int64_t cnt = 0;

       for (size_t a = 0; a < h->num_aggs; ++a) {
         const pgaccel_agg_func func = local_agg_funcs[a];
         const size_t col = local_agg_col_idx[a];
         const size_t voff = local_value_offsets[a];
         const size_t noff = local_null_offsets[a];
         const bool nullable = local_null_present[a] != 0;
         const int vtype = local_value_types[a];

         double acc = 0.0;
         if (func == PGACCEL_AGG_MIN)
           acc = std::numeric_limits<double>::infinity();
         else if (func == PGACCEL_AGG_MAX)
           acc = -std::numeric_limits<double>::infinity();

         for (size_t r = 0; r < h->row_count; ++r) {
           if (local_row_to_group[r] != g)
             continue;
           if (a == 0)
             ++cnt;

           if (func == PGACCEL_AGG_COUNT && col == SIZE_MAX) {
             acc += 1.0;
             continue;
           }

           val_read vr = device_read_value_flat(local_value_data, local_null_data, voff, noff,
                                                nullable, r, vtype);
           if (vr.is_null)
             continue;

           switch (func) {
             case PGACCEL_AGG_SUM:
               acc += vr.value;
               break;
             case PGACCEL_AGG_MIN:
               if (vr.value < acc)
                 acc = vr.value;
               break;
             case PGACCEL_AGG_MAX:
               if (vr.value > acc)
                 acc = vr.value;
               break;
             case PGACCEL_AGG_COUNT:
               acc += 1.0;
               break;
             case PGACCEL_AGG_AVG:
             case PGACCEL_AGG_STDDEV:
             case PGACCEL_AGG_VAR:
               // Partial-mode-only functions are rejected by
               // pgaccel_hash_agg_execute before this kernel launches.
               break;
           }
         }
         local_out_results[a * h->n_groups + g] = acc;
       }
       local_out_counts[g] = cnt;
     }).wait_and_throw();
  } catch (const std::exception& e) {
    std::fprintf(stderr, "pgaccel: hash_agg unsorted kernel failed: %s\n", e.what());
    sycl::free(slab, q);
    return false;
  } catch (...) {
    std::fprintf(stderr, "pgaccel: hash_agg unsorted kernel failed (unknown)\n");
    sycl::free(slab, q);
    return false;
  }
  const auto* h = reinterpret_cast<const HashAggKernelSlabHeader*>(slab);
  std::memcpy(out_results, slab + h->out_results_off, output_bytes);
  std::memcpy(out_counts, slab + h->out_counts_off, counts_bytes);
  sycl::free(slab, q);
  return true;
}

// ---------------------------------------------------------------------------
// PARTIAL-MODE accumulation kernels (Phase 3B).
//
// Output layout is the SAME flat double* for both sorted and unsorted
// paths: `out_partials[agg_offset[a] + g * width(a) + lane]`. The host
// computes `agg_offset[a]` as a prefix sum of `width(a') * n_groups`
// over a' < a and supplies it as `d_agg_offsets`. `d_agg_widths[a]`
// gives the lane width for agg a.
//
// For width-1 funcs (SUM/MIN/MAX/COUNT) the kernel writes the same
// scalar finalize-mode would have written. For AVG (width 2) it writes
// `[non_null_count, sum]`. For STDDEV/VAR (width 3) it writes
// `[non_null_count, sum, sum_sq]`.
//
// Per-row counting semantics: the per-AGG `non_null_count` counts only
// rows where this agg's value is non-null AND the row is in the group.
// The total per-group row count (NULLs included) lives in `out_counts`,
// matching finalize-mode semantics.
// ---------------------------------------------------------------------------

uint8_t* make_hashagg_kernel_slab(sycl::queue& q, size_t n_groups, size_t row_count,
                                  size_t num_aggs, const uint32_t* indices,
                                  const size_t* group_starts, const size_t* group_ends,
                                  const size_t* row_to_group, const uint8_t* value_data,
                                  size_t value_data_bytes, const uint8_t* null_data,
                                  size_t null_data_bytes, const size_t* value_offsets,
                                  const size_t* null_offsets, const uint8_t* null_present,
                                  const int* value_types, const pgaccel_agg_func* agg_funcs,
                                  const size_t* agg_col_idx, const size_t* agg_offsets,
                                  const size_t* agg_widths, size_t output_double_count) {
  HashAggKernelSlabHeader h{};
  h.n_groups = n_groups;
  h.row_count = row_count;
  h.num_aggs = num_aggs;

  size_t indices_bytes = 0;
  size_t group_bounds_bytes = 0;
  size_t row_to_group_bytes = 0;
  size_t value_offsets_bytes = 0;
  size_t null_offsets_bytes = 0;
  size_t null_present_bytes = 0;
  size_t value_types_bytes = 0;
  size_t agg_funcs_bytes = 0;
  size_t agg_col_idx_bytes = 0;
  size_t agg_offsets_bytes = 0;
  size_t agg_widths_bytes = 0;
  size_t out_results_bytes = 0;
  size_t out_counts_bytes = 0;
  if ((indices && !checked_mul_size(row_count, sizeof(uint32_t), &indices_bytes)) ||
      ((group_starts || group_ends) &&
       !checked_mul_size(n_groups, sizeof(size_t), &group_bounds_bytes)) ||
      (row_to_group && !checked_mul_size(row_count, sizeof(size_t), &row_to_group_bytes)) ||
      !checked_mul_size(num_aggs, sizeof(size_t), &value_offsets_bytes) ||
      !checked_mul_size(num_aggs, sizeof(size_t), &null_offsets_bytes) ||
      !checked_mul_size(num_aggs, sizeof(uint8_t), &null_present_bytes) ||
      !checked_mul_size(num_aggs, sizeof(int), &value_types_bytes) ||
      !checked_mul_size(num_aggs, sizeof(pgaccel_agg_func), &agg_funcs_bytes) ||
      !checked_mul_size(num_aggs, sizeof(size_t), &agg_col_idx_bytes) ||
      (agg_offsets && !checked_mul_size(num_aggs, sizeof(size_t), &agg_offsets_bytes)) ||
      (agg_widths && !checked_mul_size(num_aggs, sizeof(size_t), &agg_widths_bytes)) ||
      !checked_mul_size(output_double_count, sizeof(double), &out_results_bytes) ||
      !checked_mul_size(n_groups, sizeof(int64_t), &out_counts_bytes))
    return nullptr;

  size_t cursor = 0;
  if (!checked_align_up(sizeof(HashAggKernelSlabHeader), alignof(double), &cursor))
    return nullptr;
  bool layout_ok = true;
  auto add = [&](size_t bytes, size_t alignment) {
    size_t aligned = 0;
    if (!checked_align_up(cursor, alignment, &aligned)) {
      layout_ok = false;
      return SIZE_MAX;
    }
    const size_t span = bytes == 0 ? 1 : bytes;
    size_t next = 0;
    if (!checked_add_size(aligned, span, &next)) {
      layout_ok = false;
      return SIZE_MAX;
    }
    cursor = next;
    const size_t off = aligned;
    return off;
  };

  h.indices_off = indices ? add(indices_bytes, alignof(uint32_t)) : SIZE_MAX;
  h.group_starts_off = group_starts ? add(group_bounds_bytes, alignof(size_t)) : SIZE_MAX;
  h.group_ends_off = group_ends ? add(group_bounds_bytes, alignof(size_t)) : SIZE_MAX;
  h.row_to_group_off = row_to_group ? add(row_to_group_bytes, alignof(size_t)) : SIZE_MAX;
  h.value_data_off = add(value_data_bytes, alignof(double));
  h.null_data_off = add(null_data_bytes, alignof(double));
  h.value_offsets_off = add(value_offsets_bytes, alignof(size_t));
  h.null_offsets_off = add(null_offsets_bytes, alignof(size_t));
  h.null_present_off = add(null_present_bytes, alignof(uint8_t));
  h.value_types_off = add(value_types_bytes, alignof(int));
  h.agg_funcs_off = add(agg_funcs_bytes, alignof(pgaccel_agg_func));
  h.agg_col_idx_off = add(agg_col_idx_bytes, alignof(size_t));
  h.agg_offsets_off = agg_offsets ? add(agg_offsets_bytes, alignof(size_t)) : SIZE_MAX;
  h.agg_widths_off = agg_widths ? add(agg_widths_bytes, alignof(size_t)) : SIZE_MAX;
  h.out_results_off = add(out_results_bytes, alignof(double));
  h.out_counts_off = add(out_counts_bytes, alignof(int64_t));
  if (!layout_ok)
    return nullptr;

  uint8_t* slab = sycl::malloc_shared<uint8_t>(cursor, q);
  if (slab == nullptr)
    return nullptr;
  std::memset(slab, 0, cursor);
  std::memcpy(slab, &h, sizeof(h));

  auto copy_region = [&](size_t off, const void* src, size_t bytes) {
    if (off != SIZE_MAX && src != nullptr && bytes > 0)
      std::memcpy(slab + off, src, bytes);
  };
  copy_region(h.indices_off, indices, indices_bytes);
  copy_region(h.group_starts_off, group_starts, group_bounds_bytes);
  copy_region(h.group_ends_off, group_ends, group_bounds_bytes);
  copy_region(h.row_to_group_off, row_to_group, row_to_group_bytes);
  copy_region(h.value_data_off, value_data, value_data_bytes);
  copy_region(h.null_data_off, null_data, null_data_bytes);
  copy_region(h.value_offsets_off, value_offsets, value_offsets_bytes);
  copy_region(h.null_offsets_off, null_offsets, null_offsets_bytes);
  copy_region(h.null_present_off, null_present, null_present_bytes);
  copy_region(h.value_types_off, value_types, value_types_bytes);
  copy_region(h.agg_funcs_off, agg_funcs, agg_funcs_bytes);
  copy_region(h.agg_col_idx_off, agg_col_idx, agg_col_idx_bytes);
  copy_region(h.agg_offsets_off, agg_offsets, agg_offsets_bytes);
  copy_region(h.agg_widths_off, agg_widths, agg_widths_bytes);
  return slab;
}

bool run_unsorted_partial_kernel(sycl::queue& q, size_t n_groups, size_t row_count, size_t num_aggs,
                                 const size_t* row_to_group, const uint8_t* value_data,
                                 size_t value_data_bytes, const uint8_t* null_data,
                                 size_t null_data_bytes, const size_t* value_offsets,
                                 const size_t* null_offsets, const uint8_t* null_present,
                                 const int* value_types, const pgaccel_agg_func* agg_funcs,
                                 const size_t* agg_col_idx, const size_t* agg_offsets,
                                 const size_t* agg_widths, double* out_partials,
                                 int64_t* out_counts, size_t total_partials) {
  size_t output_bytes = 0;
  size_t counts_bytes = 0;
  if (!checked_mul_size(total_partials, sizeof(double), &output_bytes) ||
      !checked_mul_size(n_groups, sizeof(int64_t), &counts_bytes))
    return false;
  uint8_t* slab = make_hashagg_kernel_slab(
      q, n_groups, row_count, num_aggs, nullptr, nullptr, nullptr, row_to_group, value_data,
      value_data_bytes, null_data, null_data_bytes, value_offsets, null_offsets, null_present,
      value_types, agg_funcs, agg_col_idx, agg_offsets, agg_widths, total_partials);
  if (slab == nullptr)
    return false;
  try {
    q.parallel_for(sycl::range<1>(n_groups), [=](sycl::id<1> id) {
       const auto* h = reinterpret_cast<const HashAggKernelSlabHeader*>(slab);
       const auto* local_row_to_group = reinterpret_cast<const size_t*>(slab + h->row_to_group_off);
       const uint8_t* local_value_data = slab + h->value_data_off;
       const uint8_t* local_null_data = slab + h->null_data_off;
       const auto* local_value_offsets =
           reinterpret_cast<const size_t*>(slab + h->value_offsets_off);
       const auto* local_null_offsets = reinterpret_cast<const size_t*>(slab + h->null_offsets_off);
       const uint8_t* local_null_present = slab + h->null_present_off;
       const auto* local_value_types = reinterpret_cast<const int*>(slab + h->value_types_off);
       const auto* local_agg_funcs =
           reinterpret_cast<const pgaccel_agg_func*>(slab + h->agg_funcs_off);
       const auto* local_agg_col_idx = reinterpret_cast<const size_t*>(slab + h->agg_col_idx_off);
       const auto* local_agg_offsets = reinterpret_cast<const size_t*>(slab + h->agg_offsets_off);
       const auto* local_agg_widths = reinterpret_cast<const size_t*>(slab + h->agg_widths_off);
       auto* local_out_partials = reinterpret_cast<double*>(slab + h->out_results_off);
       auto* local_out_counts = reinterpret_cast<int64_t*>(slab + h->out_counts_off);
       const size_t g = id[0];
       int64_t cnt = 0;

       for (size_t a = 0; a < h->num_aggs; ++a) {
         const pgaccel_agg_func func = local_agg_funcs[a];
         const size_t col = local_agg_col_idx[a];
         const size_t voff = local_value_offsets[a];
         const size_t noff = local_null_offsets[a];
         const bool nullable = local_null_present[a] != 0;
         const int vtype = local_value_types[a];
         const size_t off = local_agg_offsets[a];
         const size_t width = local_agg_widths[a];

         double acc = 0.0;
         double sum_sq = 0.0;
         int64_t non_null_count = 0;
         if (func == PGACCEL_AGG_MIN)
           acc = std::numeric_limits<double>::infinity();
         else if (func == PGACCEL_AGG_MAX)
           acc = -std::numeric_limits<double>::infinity();

         for (size_t r = 0; r < h->row_count; ++r) {
           if (local_row_to_group[r] != g)
             continue;
           if (a == 0)
             ++cnt;

           if (func == PGACCEL_AGG_COUNT && col == SIZE_MAX) {
             acc += 1.0;
             continue;
           }

           val_read vr = device_read_value_flat(local_value_data, local_null_data, voff, noff,
                                                nullable, r, vtype);
           if (vr.is_null)
             continue;
           ++non_null_count;

           switch (func) {
             case PGACCEL_AGG_SUM:
               acc += vr.value;
               break;
             case PGACCEL_AGG_MIN:
               if (vr.value < acc)
                 acc = vr.value;
               break;
             case PGACCEL_AGG_MAX:
               if (vr.value > acc)
                 acc = vr.value;
               break;
             case PGACCEL_AGG_COUNT:
               acc += 1.0;
               break;
             case PGACCEL_AGG_AVG:
               acc += vr.value;
               break;
             case PGACCEL_AGG_STDDEV:
             case PGACCEL_AGG_VAR:
               acc += vr.value;
               sum_sq += vr.value * vr.value;
               break;
           }
         }

         // Write per-agg lanes for this group.
         double* base = local_out_partials + off + g * width;
         if (width == 1) {
           base[0] = acc;
         } else if (width == 2) {
           base[0] = static_cast<double>(non_null_count);
           base[1] = acc;
         } else if (width == 3) {
           base[0] = static_cast<double>(non_null_count);
           base[1] = acc;
           base[2] = sum_sq;
         }
       }
       local_out_counts[g] = cnt;
     }).wait_and_throw();
  } catch (const std::exception& e) {
    std::fprintf(stderr, "pgaccel: hash_agg partial unsorted kernel failed: %s\n", e.what());
    sycl::free(slab, q);
    return false;
  } catch (...) {
    std::fprintf(stderr, "pgaccel: hash_agg partial unsorted kernel failed (unknown)\n");
    sycl::free(slab, q);
    return false;
  }
  const auto* h = reinterpret_cast<const HashAggKernelSlabHeader*>(slab);
  std::memcpy(out_partials, slab + h->out_results_off, output_bytes);
  std::memcpy(out_counts, slab + h->out_counts_off, counts_bytes);
  sycl::free(slab, q);
  return true;
}

bool run_sorted_partial_kernel(sycl::queue& q, size_t n_groups, size_t num_aggs,
                               const uint32_t* indices, const size_t* group_starts,
                               const size_t* group_ends, const uint8_t* value_data,
                               size_t value_data_bytes, const uint8_t* null_data,
                               size_t null_data_bytes, const size_t* value_offsets,
                               const size_t* null_offsets, const uint8_t* null_present,
                               const int* value_types, const pgaccel_agg_func* agg_funcs,
                               const size_t* agg_col_idx, const size_t* agg_offsets,
                               const size_t* agg_widths, double* out_partials, int64_t* out_counts,
                               size_t total_partials) {
  const size_t row_count = n_groups > 0 ? group_ends[n_groups - 1] : 0;
  size_t output_bytes = 0;
  size_t counts_bytes = 0;
  if (!checked_mul_size(total_partials, sizeof(double), &output_bytes) ||
      !checked_mul_size(n_groups, sizeof(int64_t), &counts_bytes))
    return false;
  uint8_t* slab = make_hashagg_kernel_slab(
      q, n_groups, row_count, num_aggs, indices, group_starts, group_ends, nullptr, value_data,
      value_data_bytes, null_data, null_data_bytes, value_offsets, null_offsets, null_present,
      value_types, agg_funcs, agg_col_idx, agg_offsets, agg_widths, total_partials);
  if (slab == nullptr)
    return false;
  try {
    q.parallel_for(sycl::range<1>(n_groups), [=](sycl::id<1> id) {
       const auto* h = reinterpret_cast<const HashAggKernelSlabHeader*>(slab);
       const auto* local_indices = reinterpret_cast<const uint32_t*>(slab + h->indices_off);
       const auto* local_group_starts = reinterpret_cast<const size_t*>(slab + h->group_starts_off);
       const auto* local_group_ends = reinterpret_cast<const size_t*>(slab + h->group_ends_off);
       const uint8_t* local_value_data = slab + h->value_data_off;
       const uint8_t* local_null_data = slab + h->null_data_off;
       const auto* local_value_offsets =
           reinterpret_cast<const size_t*>(slab + h->value_offsets_off);
       const auto* local_null_offsets = reinterpret_cast<const size_t*>(slab + h->null_offsets_off);
       const uint8_t* local_null_present = slab + h->null_present_off;
       const auto* local_value_types = reinterpret_cast<const int*>(slab + h->value_types_off);
       const auto* local_agg_funcs =
           reinterpret_cast<const pgaccel_agg_func*>(slab + h->agg_funcs_off);
       const auto* local_agg_col_idx = reinterpret_cast<const size_t*>(slab + h->agg_col_idx_off);
       const auto* local_agg_offsets = reinterpret_cast<const size_t*>(slab + h->agg_offsets_off);
       const auto* local_agg_widths = reinterpret_cast<const size_t*>(slab + h->agg_widths_off);
       auto* local_out_partials = reinterpret_cast<double*>(slab + h->out_results_off);
       auto* local_out_counts = reinterpret_cast<int64_t*>(slab + h->out_counts_off);
       const size_t g = id[0];
       const size_t start = local_group_starts[g];
       const size_t end = local_group_ends[g];

       int64_t cnt = 0;

       for (size_t a = 0; a < h->num_aggs; ++a) {
         const pgaccel_agg_func func = local_agg_funcs[a];
         const size_t col = local_agg_col_idx[a];
         const size_t voff = local_value_offsets[a];
         const size_t noff = local_null_offsets[a];
         const bool nullable = local_null_present[a] != 0;
         const int vtype = local_value_types[a];
         const size_t off = local_agg_offsets[a];
         const size_t width = local_agg_widths[a];

         double acc = 0.0;
         double sum_sq = 0.0;
         int64_t non_null_count = 0;
         if (func == PGACCEL_AGG_MIN)
           acc = std::numeric_limits<double>::infinity();
         else if (func == PGACCEL_AGG_MAX)
           acc = -std::numeric_limits<double>::infinity();

         for (size_t i = start; i < end; ++i) {
           const uint32_t r = local_indices[i];
           if (a == 0)
             ++cnt;

           if (func == PGACCEL_AGG_COUNT && col == SIZE_MAX) {
             acc += 1.0;
             continue;
           }

           val_read vr = device_read_value_flat(local_value_data, local_null_data, voff, noff,
                                                nullable, r, vtype);
           if (vr.is_null)
             continue;
           ++non_null_count;

           switch (func) {
             case PGACCEL_AGG_SUM:
               acc += vr.value;
               break;
             case PGACCEL_AGG_MIN:
               if (vr.value < acc)
                 acc = vr.value;
               break;
             case PGACCEL_AGG_MAX:
               if (vr.value > acc)
                 acc = vr.value;
               break;
             case PGACCEL_AGG_COUNT:
               acc += 1.0;
               break;
             case PGACCEL_AGG_AVG:
               acc += vr.value;
               break;
             case PGACCEL_AGG_STDDEV:
             case PGACCEL_AGG_VAR:
               acc += vr.value;
               sum_sq += vr.value * vr.value;
               break;
           }
         }

         double* base = local_out_partials + off + g * width;
         if (width == 1) {
           base[0] = acc;
         } else if (width == 2) {
           base[0] = static_cast<double>(non_null_count);
           base[1] = acc;
         } else if (width == 3) {
           base[0] = static_cast<double>(non_null_count);
           base[1] = acc;
           base[2] = sum_sq;
         }
       }
       local_out_counts[g] = cnt;
     }).wait_and_throw();
  } catch (const std::exception& e) {
    std::fprintf(stderr, "pgaccel: hash_agg partial sorted kernel failed: %s\n", e.what());
    sycl::free(slab, q);
    return false;
  } catch (...) {
    std::fprintf(stderr, "pgaccel: hash_agg partial sorted kernel failed (unknown)\n");
    sycl::free(slab, q);
    return false;
  }
  const auto* h = reinterpret_cast<const HashAggKernelSlabHeader*>(slab);
  std::memcpy(out_partials, slab + h->out_results_off, output_bytes);
  std::memcpy(out_counts, slab + h->out_counts_off, counts_bytes);
  sycl::free(slab, q);
  return true;
}

// Helper: stage value-column data and null masks into a SINGLE flat shared
// buffer per kind (data vs nulls), with per-agg byte offsets. Returns
// owning pointers (must be sycl::free'd by caller).
//
// Per-agg pointers are NOT staged as an array of device pointers
// (`const void* const*`) because that triggers the AdaptiveCpp Metal SSCP
// MSL-emitter bug producing `device device void**` casts that fail
// `xcrun metal` validation (see commit 6de58d6 for the original repro).
// Instead all per-agg column data lives end-to-end in `d_value_data`
// and the kernel reads at `value_data + value_offsets[a] + row * elem_size`.
struct StagedColumnArrays {
  uint8_t* d_value_data;  // flat: concat of all per-agg column-data slabs
  uint8_t* d_null_data;   // flat: concat of all per-agg null-mask slabs (per-agg row_count bytes if
                          // present)
  size_t* d_value_offsets;    // [num_aggs] byte offset into d_value_data (SIZE_MAX if missing)
  size_t* d_null_offsets;     // [num_aggs] byte offset into d_null_data (SIZE_MAX if missing)
  uint8_t* d_null_present;    // [num_aggs] 0/1 flag — does this agg have a null mask
  int* d_value_types;         // [num_aggs] type tags
  pgaccel_agg_func* d_funcs;  // [num_aggs] agg funcs
  size_t* d_col_idx;          // [num_aggs] agg col indices
  size_t value_data_bytes;    // host bookkeeping for sycl::free
  size_t null_data_bytes;     // host bookkeeping for sycl::free
};

// Stage column data and null masks into shared memory. PG's column
// pointers are host-side palloc memory which may not be device-
// accessible on Metal SSCP — copy each into a single shared-mem flat
// buffer of the appropriate cumulative size based on type tags.
//
// Returns nullptr on OOM. Caller must free via free_staged_columns().
StagedColumnArrays* stage_columns(sycl::queue& q, size_t row_count, size_t num_aggs,
                                  const void* const* value_cols, const uint8_t* const* value_nulls,
                                  const int* value_types, const pgaccel_agg_col* agg_cols) {
  auto* s = new (std::nothrow) StagedColumnArrays();
  if (s == nullptr)
    return nullptr;
  s->d_value_data = nullptr;
  s->d_null_data = nullptr;
  s->value_data_bytes = 0;
  s->null_data_bytes = 0;

  s->d_value_offsets = sycl::malloc_shared<size_t>(num_aggs, q);
  s->d_null_offsets = sycl::malloc_shared<size_t>(num_aggs, q);
  s->d_null_present = sycl::malloc_shared<uint8_t>(num_aggs, q);
  s->d_value_types = sycl::malloc_shared<int>(num_aggs, q);
  s->d_funcs = sycl::malloc_shared<pgaccel_agg_func>(num_aggs, q);
  s->d_col_idx = sycl::malloc_shared<size_t>(num_aggs, q);
  auto fail = [&]() {
    if (s->d_value_data)
      sycl::free(s->d_value_data, q);
    if (s->d_null_data)
      sycl::free(s->d_null_data, q);
    if (s->d_value_offsets)
      sycl::free(s->d_value_offsets, q);
    if (s->d_null_offsets)
      sycl::free(s->d_null_offsets, q);
    if (s->d_null_present)
      sycl::free(s->d_null_present, q);
    if (s->d_value_types)
      sycl::free(s->d_value_types, q);
    if (s->d_funcs)
      sycl::free(s->d_funcs, q);
    if (s->d_col_idx)
      sycl::free(s->d_col_idx, q);
    delete s;
    return static_cast<StagedColumnArrays*>(nullptr);
  };
  if (!s->d_value_offsets || !s->d_null_offsets || !s->d_null_present || !s->d_value_types ||
      !s->d_funcs || !s->d_col_idx) {
    return fail();
  }

  // Compute per-agg byte offsets and total flat-buffer sizes.
  size_t value_total = 0;
  size_t null_total = 0;
  for (size_t a = 0; a < num_aggs; ++a) {
    s->d_value_types[a] = value_types[a];
    s->d_funcs[a] = agg_cols[a].func;
    s->d_col_idx[a] = agg_cols[a].col_idx;

    const size_t elem_size = value_elem_size(value_types[a]);
    const void* value_col = value_cols != nullptr ? value_cols[a] : nullptr;
    if (value_col == nullptr || elem_size == 0) {
      s->d_value_offsets[a] = SIZE_MAX;  // sentinel — kernel must not read
    } else {
      size_t value_bytes = 0;
      size_t next_total = 0;
      if (!checked_mul_size(row_count, elem_size, &value_bytes) ||
          !checked_add_size(value_total, value_bytes, &next_total))
        return fail();
      s->d_value_offsets[a] = value_total;
      value_total = next_total;
    }

    if (value_nulls != nullptr && value_nulls[a] != nullptr) {
      size_t next_total = 0;
      if (!checked_add_size(null_total, row_count, &next_total))
        return fail();
      s->d_null_present[a] = 1;
      s->d_null_offsets[a] = null_total;
      null_total = next_total;  // 1 byte per row
    } else {
      s->d_null_present[a] = 0;
      s->d_null_offsets[a] = SIZE_MAX;
    }
  }

  // Allocate the flat buffers (1 byte minimum to keep pointers non-null).
  s->value_data_bytes = value_total > 0 ? value_total : 1;
  s->null_data_bytes = null_total > 0 ? null_total : 1;
  s->d_value_data = sycl::malloc_shared<uint8_t>(s->value_data_bytes, q);
  s->d_null_data = sycl::malloc_shared<uint8_t>(s->null_data_bytes, q);
  if (!s->d_value_data || !s->d_null_data) {
    return fail();
  }

  // Memcpy each per-agg slab into the flat buffer at its offset.
  for (size_t a = 0; a < num_aggs; ++a) {
    const size_t voff = s->d_value_offsets[a];
    if (voff != SIZE_MAX) {
      const size_t elem_size = value_elem_size(value_types[a]);
      size_t value_bytes = 0;
      if (!checked_mul_size(row_count, elem_size, &value_bytes))
        return fail();
      const void* value_col = value_cols != nullptr ? value_cols[a] : nullptr;
      if (value_col == nullptr)
        return fail();
      std::memcpy(s->d_value_data + voff, value_col, value_bytes);
    }
    if (s->d_null_present[a]) {
      std::memcpy(s->d_null_data + s->d_null_offsets[a], value_nulls[a], row_count);
    }
  }

  return s;
}

void free_staged_columns(sycl::queue& q, StagedColumnArrays* s, size_t /*num_aggs*/) {
  if (s == nullptr)
    return;
  if (s->d_value_data)
    sycl::free(s->d_value_data, q);
  if (s->d_null_data)
    sycl::free(s->d_null_data, q);
  if (s->d_value_offsets)
    sycl::free(s->d_value_offsets, q);
  if (s->d_null_offsets)
    sycl::free(s->d_null_offsets, q);
  if (s->d_null_present)
    sycl::free(s->d_null_present, q);
  if (s->d_value_types)
    sycl::free(s->d_value_types, q);
  if (s->d_funcs)
    sycl::free(s->d_funcs, q);
  if (s->d_col_idx)
    sycl::free(s->d_col_idx, q);
  delete s;
}

}  // namespace

/// Minimum rows to consider the sort-based path. Metal currently bypasses
/// that path because AdaptiveCpp can abort the process during argument-buffer
/// validation before C++ error handling can run.
static constexpr size_t SORT_AGG_MIN_ROWS = 100000;

/// Row-parallel hash grouping path: narrow, benchmarkable vertical slice.
///
/// This path supports the planner-reopened safe case (one numeric GROUP BY key,
/// key_type 0/1/2). It avoids the legacy O(rows * groups) fallback by building
/// group IDs on GPU, compacting row indexes by group, then invoking the
/// existing per-group accumulator over contiguous group ranges.
static constexpr size_t HASH_AGG_ROW_PARALLEL_MIN_ROWS = SORT_AGG_MIN_ROWS;
static constexpr size_t HASH_AGG_ROW_PARALLEL_MAX_GROUPS =
    static_cast<size_t>(std::numeric_limits<uint32_t>::max());
static constexpr uint32_t HASH_AGG_GROUP_NONE = std::numeric_limits<uint32_t>::max();
static constexpr uint32_t HASH_AGG_SLOT_EMPTY = 0;
static constexpr uint32_t HASH_AGG_SLOT_CLAIMED = 1;
static constexpr uint32_t HASH_AGG_SLOT_FULL = 2;
static constexpr int HASH_AGG_VAL_INT64 = 3;
static constexpr int HASH_AGG_VAL_FLOAT64 = 5;

// ---------------------------------------------------------------------------
// Row-parallel hash-based grouped aggregation (group table + GPU compact).
// ---------------------------------------------------------------------------

namespace {

bool hashagg_metal_backend() {
  const pgaccel_platform_caps caps = pgaccel_get_caps();
  return std::strcmp(caps.backend_name, "metal") == 0;
}

size_t row_parallel_hashagg_group_cap() {
  return HASH_AGG_ROW_PARALLEL_MAX_GROUPS;
}

bool row_parallel_hashagg_key_supported(int key_type, size_t row_count) {
  return (key_type == 0 || key_type == 1 || key_type == 2) &&
         row_count <= static_cast<size_t>(std::numeric_limits<uint32_t>::max());
}

bool row_parallel_hashagg_supported(int key_type, size_t row_count) {
  return row_parallel_hashagg_key_supported(key_type, row_count) &&
         row_count >= HASH_AGG_ROW_PARALLEL_MIN_ROWS;
}

bool row_parallel_hashagg_agg_shape_supported(const int* value_types,
                                              const pgaccel_agg_col* agg_cols, size_t num_aggs) {
  if (value_types == nullptr || agg_cols == nullptr || num_aggs == 0)
    return false;

  for (size_t a = 0; a < num_aggs; ++a) {
    const pgaccel_agg_func func = agg_cols[a].func;
    const size_t col = agg_cols[a].col_idx;
    const bool reads_value = agg_reads_value(func, col);
    const bool value_type_ok = reads_value && (value_types[a] == HASH_AGG_VAL_INT64 ||
                                               value_types[a] == HASH_AGG_VAL_FLOAT64);

    switch (func) {
      case PGACCEL_AGG_SUM:
      case PGACCEL_AGG_MIN:
      case PGACCEL_AGG_MAX:
        if (!value_type_ok)
          return false;
        break;
      case PGACCEL_AGG_COUNT:
        if (reads_value && !value_type_ok)
          return false;
        break;
      default:
        return false;
    }
  }

  return true;
}

bool next_power_of_two_size(size_t value, size_t* out) {
  if (out == nullptr || value == 0)
    return false;
  size_t power = 1;
  while (power < value) {
    if (power > std::numeric_limits<size_t>::max() / 2)
      return false;
    power *= 2;
  }
  *out = power;
  return true;
}

static constexpr size_t HASH_AGG_SCAN_BLOCK_ROWS = 256;

// Deterministic device exclusive scan. Each work-group scans one fixed-size
// block; a small device task scans the block totals before the final offset
// add. The host only observes the completed total for bounds validation.
bool device_exclusive_scan_u32(sycl::queue& q, const uint32_t* input, size_t count,
                               uint32_t* output, uint32_t* total) {
  if (input == nullptr || output == nullptr || total == nullptr || count == 0 ||
      count > static_cast<size_t>(std::numeric_limits<uint32_t>::max()))
    return false;

  const size_t num_blocks = (count + HASH_AGG_SCAN_BLOCK_ROWS - 1) / HASH_AGG_SCAN_BLOCK_ROWS;
  uint32_t* block_totals = sycl::malloc_shared<uint32_t>(num_blocks, q);
  uint32_t* block_offsets = sycl::malloc_shared<uint32_t>(num_blocks, q);
  if (block_totals == nullptr || block_offsets == nullptr) {
    if (block_totals)
      sycl::free(block_totals, q);
    if (block_offsets)
      sycl::free(block_offsets, q);
    return false;
  }

  auto cleanup = [&]() {
    sycl::free(block_totals, q);
    sycl::free(block_offsets, q);
  };

  try {
    const auto nd = sycl::nd_range<1>(
        sycl::range<1>(num_blocks * HASH_AGG_SCAN_BLOCK_ROWS),
        sycl::range<1>(HASH_AGG_SCAN_BLOCK_ROWS));
    q.submit([&](sycl::handler& h) {
       sycl::local_accessor<uint32_t, 1> scan(sycl::range<1>(HASH_AGG_SCAN_BLOCK_ROWS), h);
       h.parallel_for(nd, [=](sycl::nd_item<1> it) {
         const size_t lid = it.get_local_id(0);
         const size_t i = it.get_global_id(0);
         scan[lid] = i < count ? input[i] : 0u;
         sycl::group_barrier(it.get_group());

         for (size_t stride = 1; stride < HASH_AGG_SCAN_BLOCK_ROWS; stride <<= 1) {
           const size_t index = (lid + 1) * stride * 2 - 1;
           if (index < HASH_AGG_SCAN_BLOCK_ROWS)
             scan[index] += scan[index - stride];
           sycl::group_barrier(it.get_group());
         }

         if (lid == 0) {
           block_totals[it.get_group(0)] = scan[HASH_AGG_SCAN_BLOCK_ROWS - 1];
           scan[HASH_AGG_SCAN_BLOCK_ROWS - 1] = 0u;
         }
         sycl::group_barrier(it.get_group());

         for (size_t stride = HASH_AGG_SCAN_BLOCK_ROWS / 2; stride > 0; stride >>= 1) {
           const size_t index = (lid + 1) * stride * 2 - 1;
           if (index < HASH_AGG_SCAN_BLOCK_ROWS) {
             const uint32_t left = scan[index - stride];
             scan[index - stride] = scan[index];
             scan[index] += left;
           }
           sycl::group_barrier(it.get_group());
         }

         if (i < count)
           output[i] = scan[lid];
       });
     }).wait_and_throw();

    q.single_task([=]() {
       uint32_t cursor = 0;
       for (size_t block = 0; block < num_blocks; ++block) {
         block_offsets[block] = cursor;
         cursor += block_totals[block];
       }
       total[0] = cursor;
     }).wait_and_throw();

    q.parallel_for(sycl::range<1>(count), [=](sycl::id<1> id) {
       const size_t i = id[0];
       output[i] += block_offsets[i / HASH_AGG_SCAN_BLOCK_ROWS];
     }).wait_and_throw();
    pgaccel_record_gpu_exec();
  } catch (const std::exception& e) {
    std::fprintf(stderr, "pgaccel: hash_agg device scan failed: %s\n", e.what());
    cleanup();
    return false;
  } catch (...) {
    std::fprintf(stderr, "pgaccel: hash_agg device scan failed (unknown)\n");
    cleanup();
    return false;
  }

  cleanup();
  return true;
}

static constexpr uint64_t HASH_AGG_SLOT_BITS_EMPTY = std::numeric_limits<uint64_t>::max();

inline uint64_t group_key_bits(int32_t key) {
  return static_cast<uint64_t>(static_cast<uint32_t>(key));
}

inline uint64_t group_key_bits(int64_t key) {
  return static_cast<uint64_t>(key);
}

inline uint64_t group_key_bits(double key) {
  if (key != key)
    return 0x7FF8000000000000ULL;
  if (key == 0.0)
    key = 0.0;
  union {
    double d;
    uint64_t u;
  } bits;
  bits.d = key;
  return bits.u;
}

template <typename KeyT>
KeyT group_key_from_bits(uint64_t bits) {
  return static_cast<KeyT>(bits);
}

template <>
int32_t group_key_from_bits<int32_t>(uint64_t bits) {
  return static_cast<int32_t>(static_cast<uint32_t>(bits));
}

template <>
double group_key_from_bits<double>(uint64_t bits) {
  return sycl::bit_cast<double>(bits);
}

inline uint64_t device_hash_key(int32_t key) {
  return hash64(group_key_bits(key));
}

inline uint64_t device_hash_key(int64_t key) {
  return hash64(group_key_bits(key));
}

inline uint64_t device_hash_key(double key) {
  return hash64(group_key_bits(key));
}

template <typename KeyT>
inline bool device_key_equal(KeyT a, KeyT b) {
  return a == b;
}

template <>
inline bool device_key_equal<double>(double a, double b) {
  const bool a_nan = a != a;
  const bool b_nan = b != b;
  return (a_nan && b_nan) || a == b;
}

template <typename KeyT>
bool run_numeric_group_hash_kernel(sycl::queue& q, const void* group_keys,
                                   const uint8_t* group_null_mask, size_t row_count,
                                   uint32_t* row_to_group, uint32_t* group_counts,
                                   KeyT* group_key_out, uint32_t* out_group_count) {
  if (group_keys == nullptr || row_to_group == nullptr || group_counts == nullptr ||
      group_key_out == nullptr || out_group_count == nullptr)
    return false;

  size_t table_need = 0;
  size_t table_capacity = 0;
  if (!checked_mul_size(row_count, 2, &table_need) ||
      !next_power_of_two_size(table_need, &table_capacity))
    return false;
  if (table_capacity > static_cast<size_t>(std::numeric_limits<uint32_t>::max()))
    return false;

  KeyT* d_keys = sycl::malloc_shared<KeyT>(row_count, q);
  uint8_t* d_key_nulls = sycl::malloc_shared<uint8_t>(row_count > 0 ? row_count : 1, q);
  uint32_t* d_slot_states = sycl::malloc_shared<uint32_t>(table_capacity, q);
  KeyT* d_slot_keys = sycl::malloc_shared<KeyT>(table_capacity, q);
  uint32_t* d_slot_groups = sycl::malloc_shared<uint32_t>(table_capacity, q);
  uint32_t* d_null_state = sycl::malloc_shared<uint32_t>(1, q);
  uint32_t* d_null_group = sycl::malloc_shared<uint32_t>(1, q);
  if (d_keys == nullptr || d_key_nulls == nullptr || d_slot_states == nullptr ||
      d_slot_keys == nullptr || d_slot_groups == nullptr || d_null_state == nullptr ||
      d_null_group == nullptr) {
    if (d_keys)
      sycl::free(d_keys, q);
    if (d_key_nulls)
      sycl::free(d_key_nulls, q);
    if (d_slot_states)
      sycl::free(d_slot_states, q);
    if (d_slot_keys)
      sycl::free(d_slot_keys, q);
    if (d_slot_groups)
      sycl::free(d_slot_groups, q);
    if (d_null_state)
      sycl::free(d_null_state, q);
    if (d_null_group)
      sycl::free(d_null_group, q);
    return false;
  }

  std::memcpy(d_keys, group_keys, row_count * sizeof(KeyT));
  if (group_null_mask != nullptr)
    std::memcpy(d_key_nulls, group_null_mask, row_count);
  else
    std::memset(d_key_nulls, 0, row_count);
  std::memset(d_slot_states, 0, table_capacity * sizeof(uint32_t));
  std::memset(d_slot_keys, 0, table_capacity * sizeof(KeyT));
  std::memset(d_slot_groups, 0, table_capacity * sizeof(uint32_t));
  std::memset(group_counts, 0, row_count * sizeof(uint32_t));
  std::memset(group_key_out, 0, row_count * sizeof(KeyT));
  *out_group_count = 0;
  *d_null_state = HASH_AGG_SLOT_EMPTY;
  *d_null_group = HASH_AGG_GROUP_NONE;

  const uint32_t mask = static_cast<uint32_t>(table_capacity - 1);
  const bool has_null_mask = group_null_mask != nullptr;

  try {
    q.parallel_for(sycl::range<1>(row_count), [=](sycl::id<1> id) {
       const uint32_t r = static_cast<uint32_t>(id[0]);
       uint32_t group_id = HASH_AGG_GROUP_NONE;

       if (has_null_mask && d_key_nulls[r] != 0) {
         sycl::atomic_ref<uint32_t, sycl::memory_order::relaxed, sycl::memory_scope::device,
                          sycl::access::address_space::global_space>
             null_state_ref(d_null_state[0]);
         uint32_t state = null_state_ref.load();
         if (state == HASH_AGG_SLOT_EMPTY) {
           uint32_t expected = HASH_AGG_SLOT_EMPTY;
           if (null_state_ref.compare_exchange_strong(expected, HASH_AGG_SLOT_CLAIMED)) {
             sycl::atomic_ref<uint32_t, sycl::memory_order::relaxed, sycl::memory_scope::device,
                              sycl::access::address_space::global_space>
                 group_counter_ref(out_group_count[0]);
             group_id = group_counter_ref.fetch_add(1u);
             d_null_group[0] = group_id;
             group_key_out[group_id] = KeyT{};
             sycl::atomic_fence(sycl::memory_order::release, sycl::memory_scope::device);
             null_state_ref.store(HASH_AGG_SLOT_FULL);
           }
         }
         while (group_id == HASH_AGG_GROUP_NONE) {
           state = null_state_ref.load();
           if (state == HASH_AGG_SLOT_FULL) {
             sycl::atomic_fence(sycl::memory_order::acquire, sycl::memory_scope::device);
             group_id = d_null_group[0];
           }
         }
       } else {
         const KeyT key = d_keys[r];
         uint32_t slot = static_cast<uint32_t>(device_hash_key(key)) & mask;

         for (uint32_t probe = 0; probe <= mask && group_id == HASH_AGG_GROUP_NONE; ++probe) {
           sycl::atomic_ref<uint32_t, sycl::memory_order::relaxed, sycl::memory_scope::device,
                            sycl::access::address_space::global_space>
               slot_state_ref(d_slot_states[slot]);
           uint32_t state = slot_state_ref.load();
           if (state == HASH_AGG_SLOT_EMPTY) {
             uint32_t expected = HASH_AGG_SLOT_EMPTY;
             if (slot_state_ref.compare_exchange_strong(expected, HASH_AGG_SLOT_CLAIMED)) {
               sycl::atomic_ref<uint32_t, sycl::memory_order::relaxed, sycl::memory_scope::device,
                                sycl::access::address_space::global_space>
                   group_counter_ref(out_group_count[0]);
               group_id = group_counter_ref.fetch_add(1u);
               d_slot_keys[slot] = key;
               d_slot_groups[slot] = group_id;
               group_key_out[group_id] = key;
               sycl::atomic_fence(sycl::memory_order::release, sycl::memory_scope::device);
               slot_state_ref.store(HASH_AGG_SLOT_FULL);
               break;
             }
           }

           while (slot_state_ref.load() == HASH_AGG_SLOT_CLAIMED) {}
           if (slot_state_ref.load() == HASH_AGG_SLOT_FULL) {
             sycl::atomic_fence(sycl::memory_order::acquire, sycl::memory_scope::device);
             if (device_key_equal(d_slot_keys[slot], key)) {
               group_id = d_slot_groups[slot];
               break;
             }
           }
           slot = (slot + 1u) & mask;
         }
       }

       row_to_group[r] = group_id;
       if (group_id != HASH_AGG_GROUP_NONE) {
         sycl::atomic_ref<uint32_t, sycl::memory_order::relaxed, sycl::memory_scope::device,
                          sycl::access::address_space::global_space>
             count_ref(group_counts[group_id]);
         count_ref.fetch_add(1u);
       }
     }).wait_and_throw();
  } catch (const std::exception& e) {
    std::fprintf(stderr, "pgaccel: hash_agg row-parallel grouping kernel failed: %s\n", e.what());
    sycl::free(d_keys, q);
    sycl::free(d_key_nulls, q);
    sycl::free(d_slot_states, q);
    sycl::free(d_slot_keys, q);
    sycl::free(d_slot_groups, q);
    sycl::free(d_null_state, q);
    sycl::free(d_null_group, q);
    return false;
  } catch (...) {
    std::fprintf(stderr, "pgaccel: hash_agg row-parallel grouping kernel failed (unknown)\n");
    sycl::free(d_keys, q);
    sycl::free(d_key_nulls, q);
    sycl::free(d_slot_states, q);
    sycl::free(d_slot_keys, q);
    sycl::free(d_slot_groups, q);
    sycl::free(d_null_state, q);
    sycl::free(d_null_group, q);
    return false;
  }

  sycl::free(d_keys, q);
  sycl::free(d_key_nulls, q);
  sycl::free(d_slot_states, q);
  sycl::free(d_slot_keys, q);
  sycl::free(d_slot_groups, q);
  sycl::free(d_null_state, q);
  sycl::free(d_null_group, q);
  return true;
}

template <typename KeyT>
bool run_numeric_group_hash_kernel_nospin(sycl::queue& q, const void* group_keys,
                                          const uint8_t* group_null_mask, size_t row_count,
                                          uint32_t* row_to_group, uint32_t* group_counts,
                                          KeyT* group_key_out, uint32_t* out_group_count) {
  if (group_keys == nullptr || row_to_group == nullptr || group_counts == nullptr ||
      group_key_out == nullptr || out_group_count == nullptr)
    return false;

  size_t table_need = 0;
  size_t table_capacity = 0;
  if (!checked_mul_size(row_count, 2, &table_need) ||
      !next_power_of_two_size(table_need, &table_capacity))
    return false;
  if (table_capacity > static_cast<size_t>(std::numeric_limits<uint32_t>::max()))
    return false;

  size_t slot_count = 0;
  if (!checked_add_size(table_capacity, 2, &slot_count) ||
      slot_count > static_cast<size_t>(std::numeric_limits<uint32_t>::max()))
    return false;
  const uint32_t table_slots = static_cast<uint32_t>(table_capacity);
  const uint32_t special_bits_slot = table_slots;
  const uint32_t null_slot = table_slots + 1u;

  KeyT* d_keys = sycl::malloc_shared<KeyT>(row_count, q);
  uint8_t* d_key_nulls = sycl::malloc_shared<uint8_t>(row_count > 0 ? row_count : 1, q);
  uint64_t* d_slot_bits = sycl::malloc_shared<uint64_t>(slot_count, q);
  uint32_t* d_slot_counts = sycl::malloc_shared<uint32_t>(slot_count, q);
  uint32_t* d_overflow = sycl::malloc_shared<uint32_t>(1, q);
  if (d_keys == nullptr || d_key_nulls == nullptr || d_slot_bits == nullptr ||
      d_slot_counts == nullptr || d_overflow == nullptr) {
    if (d_keys)
      sycl::free(d_keys, q);
    if (d_key_nulls)
      sycl::free(d_key_nulls, q);
    if (d_slot_bits)
      sycl::free(d_slot_bits, q);
    if (d_slot_counts)
      sycl::free(d_slot_counts, q);
    if (d_overflow)
      sycl::free(d_overflow, q);
    return false;
  }

  std::memcpy(d_keys, group_keys, row_count * sizeof(KeyT));
  if (group_null_mask != nullptr)
    std::memcpy(d_key_nulls, group_null_mask, row_count);
  else
    std::memset(d_key_nulls, 0, row_count);
  std::fill(d_slot_bits, d_slot_bits + slot_count, HASH_AGG_SLOT_BITS_EMPTY);
  std::memset(d_slot_counts, 0, slot_count * sizeof(uint32_t));
  std::memset(group_counts, 0, row_count * sizeof(uint32_t));
  std::memset(group_key_out, 0, row_count * sizeof(KeyT));
  *d_overflow = 0;
  *out_group_count = 0;

  const uint32_t mask = static_cast<uint32_t>(table_capacity - 1);
  const bool has_null_mask = group_null_mask != nullptr;

  try {
    q.parallel_for(sycl::range<1>(row_count), [=](sycl::id<1> id) {
       const uint32_t r = static_cast<uint32_t>(id[0]);
       uint32_t slot_id = HASH_AGG_GROUP_NONE;

       if (has_null_mask && d_key_nulls[r] != 0) {
         slot_id = null_slot;
       } else {
         const uint64_t bits = group_key_bits(d_keys[r]);
         if (bits == HASH_AGG_SLOT_BITS_EMPTY) {
           slot_id = special_bits_slot;
         } else {
           uint32_t slot = static_cast<uint32_t>(hash64(bits)) & mask;
           for (uint32_t probe = 0; probe <= mask; ++probe) {
             sycl::atomic_ref<uint64_t, sycl::memory_order::relaxed, sycl::memory_scope::device,
                              sycl::access::address_space::global_space>
                 slot_bits_ref(d_slot_bits[slot]);
             const uint64_t current = slot_bits_ref.load();
             if (current == bits) {
               slot_id = slot;
               break;
             }
             if (current == HASH_AGG_SLOT_BITS_EMPTY) {
               uint64_t expected = HASH_AGG_SLOT_BITS_EMPTY;
               if (slot_bits_ref.compare_exchange_strong(expected, bits) || expected == bits) {
                 slot_id = slot;
                 break;
               }
             }
             slot = (slot + 1u) & mask;
           }
         }
       }

       row_to_group[r] = slot_id;
       if (slot_id == HASH_AGG_GROUP_NONE) {
         sycl::atomic_ref<uint32_t, sycl::memory_order::relaxed, sycl::memory_scope::device,
                          sycl::access::address_space::global_space>
             overflow_ref(d_overflow[0]);
         overflow_ref.store(1u);
         return;
       }

       sycl::atomic_ref<uint32_t, sycl::memory_order::relaxed, sycl::memory_scope::device,
                        sycl::access::address_space::global_space>
           count_ref(d_slot_counts[slot_id]);
       count_ref.fetch_add(1u);
     }).wait_and_throw();
  } catch (const std::exception& e) {
    std::fprintf(stderr, "pgaccel: hash_agg row-parallel grouping kernel failed: %s\n", e.what());
    sycl::free(d_keys, q);
    sycl::free(d_key_nulls, q);
    sycl::free(d_slot_bits, q);
    sycl::free(d_slot_counts, q);
    sycl::free(d_overflow, q);
    return false;
  } catch (...) {
    std::fprintf(stderr, "pgaccel: hash_agg row-parallel grouping kernel failed (unknown)\n");
    sycl::free(d_keys, q);
    sycl::free(d_key_nulls, q);
    sycl::free(d_slot_bits, q);
    sycl::free(d_slot_counts, q);
    sycl::free(d_overflow, q);
    return false;
  }

  if (*d_overflow != 0) {
    sycl::free(d_keys, q);
    sycl::free(d_key_nulls, q);
    sycl::free(d_slot_bits, q);
    sycl::free(d_slot_counts, q);
    sycl::free(d_overflow, q);
    return false;
  }

  uint32_t* d_slot_present = sycl::malloc_shared<uint32_t>(slot_count, q);
  uint32_t* d_slot_to_group = sycl::malloc_shared<uint32_t>(slot_count, q);
  if (d_slot_present == nullptr || d_slot_to_group == nullptr) {
    if (d_slot_present)
      sycl::free(d_slot_present, q);
    if (d_slot_to_group)
      sycl::free(d_slot_to_group, q);
    sycl::free(d_keys, q);
    sycl::free(d_key_nulls, q);
    sycl::free(d_slot_bits, q);
    sycl::free(d_slot_counts, q);
    sycl::free(d_overflow, q);
    return false;
  }

  try {
    q.parallel_for(sycl::range<1>(slot_count), [=](sycl::id<1> id) {
       const size_t slot = id[0];
       d_slot_present[slot] = d_slot_counts[slot] != 0 ? 1u : 0u;
     }).wait_and_throw();
  } catch (const std::exception& e) {
    std::fprintf(stderr, "pgaccel: hash_agg row-parallel occupancy kernel failed: %s\n",
                 e.what());
    sycl::free(d_slot_present, q);
    sycl::free(d_slot_to_group, q);
    sycl::free(d_keys, q);
    sycl::free(d_key_nulls, q);
    sycl::free(d_slot_bits, q);
    sycl::free(d_slot_counts, q);
    sycl::free(d_overflow, q);
    return false;
  } catch (...) {
    std::fprintf(stderr, "pgaccel: hash_agg row-parallel occupancy kernel failed (unknown)\n");
    sycl::free(d_slot_present, q);
    sycl::free(d_slot_to_group, q);
    sycl::free(d_keys, q);
    sycl::free(d_key_nulls, q);
    sycl::free(d_slot_bits, q);
    sycl::free(d_slot_counts, q);
    sycl::free(d_overflow, q);
    return false;
  }

  if (!device_exclusive_scan_u32(q, d_slot_present, slot_count, d_slot_to_group,
                                 out_group_count)) {
    sycl::free(d_slot_present, q);
    sycl::free(d_slot_to_group, q);
    sycl::free(d_keys, q);
    sycl::free(d_key_nulls, q);
    sycl::free(d_slot_bits, q);
    sycl::free(d_slot_counts, q);
    sycl::free(d_overflow, q);
    return false;
  }

  const uint32_t n_groups = *out_group_count;
  if (n_groups == 0 || static_cast<size_t>(n_groups) > row_count) {
    sycl::free(d_slot_present, q);
    sycl::free(d_slot_to_group, q);
    sycl::free(d_keys, q);
    sycl::free(d_key_nulls, q);
    sycl::free(d_slot_bits, q);
    sycl::free(d_slot_counts, q);
    sycl::free(d_overflow, q);
    return false;
  }

  try {
    q.parallel_for(sycl::range<1>(slot_count), [=](sycl::id<1> id) {
       const size_t slot = id[0];
       if (d_slot_present[slot] == 0)
         return;
       const uint32_t group_id = d_slot_to_group[slot];
       group_counts[group_id] = d_slot_counts[slot];
       if (slot == static_cast<size_t>(null_slot)) {
         group_key_out[group_id] = KeyT{};
       } else if (slot == static_cast<size_t>(special_bits_slot)) {
         group_key_out[group_id] = group_key_from_bits<KeyT>(HASH_AGG_SLOT_BITS_EMPTY);
       } else {
         group_key_out[group_id] = group_key_from_bits<KeyT>(d_slot_bits[slot]);
       }
     }).wait_and_throw();

    q.parallel_for(sycl::range<1>(row_count), [=](sycl::id<1> id) {
       const uint32_t r = static_cast<uint32_t>(id[0]);
       const uint32_t slot = row_to_group[r];
       if (slot != HASH_AGG_GROUP_NONE)
         row_to_group[r] = d_slot_to_group[slot];
     }).wait_and_throw();
  } catch (const std::exception& e) {
    std::fprintf(stderr, "pgaccel: hash_agg row-parallel remap kernel failed: %s\n", e.what());
    sycl::free(d_slot_present, q);
    sycl::free(d_slot_to_group, q);
    sycl::free(d_keys, q);
    sycl::free(d_key_nulls, q);
    sycl::free(d_slot_bits, q);
    sycl::free(d_slot_counts, q);
    sycl::free(d_overflow, q);
    return false;
  } catch (...) {
    std::fprintf(stderr, "pgaccel: hash_agg row-parallel remap kernel failed (unknown)\n");
    sycl::free(d_slot_present, q);
    sycl::free(d_slot_to_group, q);
    sycl::free(d_keys, q);
    sycl::free(d_key_nulls, q);
    sycl::free(d_slot_bits, q);
    sycl::free(d_slot_counts, q);
    sycl::free(d_overflow, q);
    return false;
  }

  sycl::free(d_slot_present, q);
  sycl::free(d_slot_to_group, q);
  sycl::free(d_keys, q);
  sycl::free(d_key_nulls, q);
  sycl::free(d_slot_bits, q);
  sycl::free(d_slot_counts, q);
  sycl::free(d_overflow, q);
  return true;
}

bool run_group_scatter_kernel(sycl::queue& q, size_t row_count, const uint32_t* row_to_group,
                              uint32_t* scatter_offsets, uint32_t* compacted_indices) {
  if (row_to_group == nullptr || scatter_offsets == nullptr || compacted_indices == nullptr)
    return false;
  try {
    q.parallel_for(sycl::range<1>(row_count), [=](sycl::id<1> id) {
       const uint32_t r = static_cast<uint32_t>(id[0]);
       const uint32_t g = row_to_group[r];
       if (g == HASH_AGG_GROUP_NONE)
         return;
       sycl::atomic_ref<uint32_t, sycl::memory_order::relaxed, sycl::memory_scope::device,
                        sycl::access::address_space::global_space>
           off_ref(scatter_offsets[g]);
       const uint32_t pos = off_ref.fetch_add(1u);
       compacted_indices[pos] = r;
     }).wait_and_throw();
  } catch (const std::exception& e) {
    std::fprintf(stderr, "pgaccel: hash_agg row scatter kernel failed: %s\n", e.what());
    return false;
  } catch (...) {
    std::fprintf(stderr, "pgaccel: hash_agg row scatter kernel failed (unknown)\n");
    return false;
  }
  return true;
}

template <typename KeyT>
static pgaccel_agg_state* agg_hash_row_parallel_numeric(
    const void* group_keys, const uint8_t* group_null_mask, size_t row_count, int key_type,
    const void* const* value_cols, const uint8_t* const* value_nulls, const int* value_types,
    const pgaccel_agg_col* agg_cols, size_t num_aggs, bool enforce_min_rows = true) {
  sycl::queue* q = pgaccel_get_queue();
  if (q == nullptr)
    return nullptr;
  if (hashagg_metal_backend())
    return nullptr;
  if (!row_parallel_hashagg_key_supported(key_type, row_count))
    return nullptr;
  if (enforce_min_rows && row_count < HASH_AGG_ROW_PARALLEL_MIN_ROWS)
    return nullptr;
  if (!row_parallel_hashagg_agg_shape_supported(value_types, agg_cols, num_aggs))
    return nullptr;

  uint32_t* d_row_to_group = sycl::malloc_shared<uint32_t>(row_count, *q);
  uint32_t* d_group_counts = sycl::malloc_shared<uint32_t>(row_count, *q);
  KeyT* d_group_keys = sycl::malloc_shared<KeyT>(row_count, *q);
  uint32_t* d_group_count = sycl::malloc_shared<uint32_t>(1, *q);
  if (d_row_to_group == nullptr || d_group_counts == nullptr || d_group_keys == nullptr ||
      d_group_count == nullptr) {
    if (d_row_to_group)
      sycl::free(d_row_to_group, *q);
    if (d_group_counts)
      sycl::free(d_group_counts, *q);
    if (d_group_keys)
      sycl::free(d_group_keys, *q);
    if (d_group_count)
      sycl::free(d_group_count, *q);
    return nullptr;
  }

  const bool grouped = run_numeric_group_hash_kernel_nospin<KeyT>(
      *q, group_keys, group_null_mask, row_count, d_row_to_group, d_group_counts, d_group_keys,
      d_group_count);
  if (!grouped) {
    sycl::free(d_row_to_group, *q);
    sycl::free(d_group_counts, *q);
    sycl::free(d_group_keys, *q);
    sycl::free(d_group_count, *q);
    return nullptr;
  }

  const size_t n_groups = static_cast<size_t>(*d_group_count);
  const size_t max_groups = row_parallel_hashagg_group_cap();
  if (n_groups == 0 || n_groups > max_groups || n_groups > row_count) {
    std::fprintf(stderr,
                 "pgaccel: hash_agg row-parallel grouping rejected after build "
                 "(rows=%zu groups=%zu max=%zu)\n",
                 row_count, n_groups, max_groups);
    sycl::free(d_row_to_group, *q);
    sycl::free(d_group_counts, *q);
    sycl::free(d_group_keys, *q);
    sycl::free(d_group_count, *q);
    return nullptr;
  }

  size_t* d_group_starts = sycl::malloc_shared<size_t>(n_groups, *q);
  size_t* d_group_ends = sycl::malloc_shared<size_t>(n_groups, *q);
  uint32_t* d_scatter_offsets = sycl::malloc_shared<uint32_t>(n_groups, *q);
  uint32_t* d_indices = sycl::malloc_shared<uint32_t>(row_count, *q);
  if (d_group_starts == nullptr || d_group_ends == nullptr || d_scatter_offsets == nullptr ||
      d_indices == nullptr) {
    if (d_group_starts)
      sycl::free(d_group_starts, *q);
    if (d_group_ends)
      sycl::free(d_group_ends, *q);
    if (d_scatter_offsets)
      sycl::free(d_scatter_offsets, *q);
    if (d_indices)
      sycl::free(d_indices, *q);
    sycl::free(d_row_to_group, *q);
    sycl::free(d_group_counts, *q);
    sycl::free(d_group_keys, *q);
    sycl::free(d_group_count, *q);
    return nullptr;
  }

  if (!device_exclusive_scan_u32(*q, d_group_counts, n_groups, d_scatter_offsets,
                                 d_group_count) ||
      static_cast<size_t>(*d_group_count) != row_count) {
    std::fprintf(stderr,
                 "pgaccel: hash_agg row-parallel grouping count mismatch "
                 "(rows=%zu compacted=%u groups=%zu)\n",
                 row_count, *d_group_count, n_groups);
    sycl::free(d_group_starts, *q);
    sycl::free(d_group_ends, *q);
    sycl::free(d_scatter_offsets, *q);
    sycl::free(d_indices, *q);
    sycl::free(d_row_to_group, *q);
    sycl::free(d_group_counts, *q);
    sycl::free(d_group_keys, *q);
    sycl::free(d_group_count, *q);
    return nullptr;
  }

  try {
    q->parallel_for(sycl::range<1>(n_groups), [=](sycl::id<1> id) {
       const size_t group = id[0];
       const size_t start = static_cast<size_t>(d_scatter_offsets[group]);
       d_group_starts[group] = start;
       d_group_ends[group] = start + static_cast<size_t>(d_group_counts[group]);
     }).wait_and_throw();
  } catch (const std::exception& e) {
    std::fprintf(stderr, "pgaccel: hash_agg row-parallel boundary kernel failed: %s\n",
                 e.what());
    sycl::free(d_group_starts, *q);
    sycl::free(d_group_ends, *q);
    sycl::free(d_scatter_offsets, *q);
    sycl::free(d_indices, *q);
    sycl::free(d_row_to_group, *q);
    sycl::free(d_group_counts, *q);
    sycl::free(d_group_keys, *q);
    sycl::free(d_group_count, *q);
    return nullptr;
  } catch (...) {
    std::fprintf(stderr, "pgaccel: hash_agg row-parallel boundary kernel failed (unknown)\n");
    sycl::free(d_group_starts, *q);
    sycl::free(d_group_ends, *q);
    sycl::free(d_scatter_offsets, *q);
    sycl::free(d_indices, *q);
    sycl::free(d_row_to_group, *q);
    sycl::free(d_group_counts, *q);
    sycl::free(d_group_keys, *q);
    sycl::free(d_group_count, *q);
    return nullptr;
  }

  if (!run_group_scatter_kernel(*q, row_count, d_row_to_group, d_scatter_offsets, d_indices)) {
    sycl::free(d_group_starts, *q);
    sycl::free(d_group_ends, *q);
    sycl::free(d_scatter_offsets, *q);
    sycl::free(d_indices, *q);
    sycl::free(d_row_to_group, *q);
    sycl::free(d_group_counts, *q);
    sycl::free(d_group_keys, *q);
    sycl::free(d_group_count, *q);
    return nullptr;
  }

  StagedColumnArrays* sc =
      stage_columns(*q, row_count, num_aggs, value_cols, value_nulls, value_types, agg_cols);
  if (sc == nullptr) {
    sycl::free(d_group_starts, *q);
    sycl::free(d_group_ends, *q);
    sycl::free(d_scatter_offsets, *q);
    sycl::free(d_indices, *q);
    sycl::free(d_row_to_group, *q);
    sycl::free(d_group_counts, *q);
    sycl::free(d_group_keys, *q);
    sycl::free(d_group_count, *q);
    return nullptr;
  }

  size_t result_count = 0;
  if (!checked_mul_size(num_aggs, n_groups, &result_count)) {
    free_staged_columns(*q, sc, num_aggs);
    sycl::free(d_group_starts, *q);
    sycl::free(d_group_ends, *q);
    sycl::free(d_scatter_offsets, *q);
    sycl::free(d_indices, *q);
    sycl::free(d_row_to_group, *q);
    sycl::free(d_group_counts, *q);
    sycl::free(d_group_keys, *q);
    sycl::free(d_group_count, *q);
    return nullptr;
  }

  double* d_results = sycl::malloc_shared<double>(result_count, *q);
  int64_t* d_counts = sycl::malloc_shared<int64_t>(n_groups, *q);
  if (d_results == nullptr || d_counts == nullptr) {
    if (d_results)
      sycl::free(d_results, *q);
    if (d_counts)
      sycl::free(d_counts, *q);
    free_staged_columns(*q, sc, num_aggs);
    sycl::free(d_group_starts, *q);
    sycl::free(d_group_ends, *q);
    sycl::free(d_scatter_offsets, *q);
    sycl::free(d_indices, *q);
    sycl::free(d_row_to_group, *q);
    sycl::free(d_group_counts, *q);
    sycl::free(d_group_keys, *q);
    sycl::free(d_group_count, *q);
    return nullptr;
  }

  if (!run_sorted_accum_kernel(*q, n_groups, num_aggs, d_indices, d_group_starts, d_group_ends,
                               sc->d_value_data, sc->value_data_bytes, sc->d_null_data,
                               sc->null_data_bytes, sc->d_value_offsets, sc->d_null_offsets,
                               sc->d_null_present, sc->d_value_types, sc->d_funcs, sc->d_col_idx,
                               d_results, d_counts)) {
    sycl::free(d_results, *q);
    sycl::free(d_counts, *q);
    free_staged_columns(*q, sc, num_aggs);
    sycl::free(d_group_starts, *q);
    sycl::free(d_group_ends, *q);
    sycl::free(d_scatter_offsets, *q);
    sycl::free(d_indices, *q);
    sycl::free(d_row_to_group, *q);
    sycl::free(d_group_counts, *q);
    sycl::free(d_group_keys, *q);
    sycl::free(d_group_count, *q);
    return nullptr;
  }

  auto* state = new (std::nothrow) pgaccel_agg_state();
  if (state == nullptr) {
    sycl::free(d_results, *q);
    sycl::free(d_counts, *q);
    free_staged_columns(*q, sc, num_aggs);
    sycl::free(d_group_starts, *q);
    sycl::free(d_group_ends, *q);
    sycl::free(d_scatter_offsets, *q);
    sycl::free(d_indices, *q);
    sycl::free(d_row_to_group, *q);
    sycl::free(d_group_counts, *q);
    sycl::free(d_group_keys, *q);
    sycl::free(d_group_count, *q);
    return nullptr;
  }
  state->key_size = sizeof(KeyT);
  state->key_type = key_type;
  state->group_count = n_groups;
  state->num_aggs = num_aggs;
  state->group_key_buf.resize(n_groups * sizeof(KeyT));
  std::memcpy(state->group_key_buf.data(), d_group_keys, n_groups * sizeof(KeyT));
  state->counts.assign(d_counts, d_counts + n_groups);
  state->results.resize(num_aggs);
  for (size_t a = 0; a < num_aggs; ++a) {
    state->results[a].assign(d_results + a * n_groups, d_results + (a + 1) * n_groups);
  }

  sycl::free(d_results, *q);
  sycl::free(d_counts, *q);
  free_staged_columns(*q, sc, num_aggs);
  sycl::free(d_group_starts, *q);
  sycl::free(d_group_ends, *q);
  sycl::free(d_scatter_offsets, *q);
  sycl::free(d_indices, *q);
  sycl::free(d_row_to_group, *q);
  sycl::free(d_group_counts, *q);
  sycl::free(d_group_keys, *q);
  sycl::free(d_group_count, *q);
  pgaccel_record_gpu_exec();
  return state;
}

static pgaccel_agg_state* agg_hash_row_parallel(const void* group_keys,
                                                const uint8_t* group_null_mask, size_t row_count,
                                                int key_type, const void* const* value_cols,
                                                const uint8_t* const* value_nulls,
                                                const int* value_types,
                                                const pgaccel_agg_col* agg_cols, size_t num_aggs) {
  switch (key_type) {
    case 0:
      return agg_hash_row_parallel_numeric<int32_t>(group_keys, group_null_mask, row_count,
                                                    key_type, value_cols, value_nulls, value_types,
                                                    agg_cols, num_aggs);
    case 1:
      return agg_hash_row_parallel_numeric<int64_t>(group_keys, group_null_mask, row_count,
                                                    key_type, value_cols, value_nulls, value_types,
                                                    agg_cols, num_aggs);
    case 2:
      return agg_hash_row_parallel_numeric<double>(group_keys, group_null_mask, row_count, key_type,
                                                   value_cols, value_nulls, value_types, agg_cols,
                                                   num_aggs);
    default:
      return nullptr;
  }
}

static pgaccel_agg_state* agg_count_i64_hash_device(int64_t* group_keys, size_t row_count,
                                                    size_t max_distinct_hint) {
  sycl::queue* q = pgaccel_get_queue();
  if (q == nullptr || group_keys == nullptr || row_count == 0 ||
      row_count > static_cast<size_t>(std::numeric_limits<uint32_t>::max()))
    return nullptr;

  size_t max_distinct = max_distinct_hint;
  if (max_distinct == 0 || max_distinct > row_count)
    max_distinct = row_count;

  size_t table_need = 0;
  size_t table_capacity = 0;
  if (!checked_mul_size(max_distinct, 2, &table_need) ||
      !next_power_of_two_size(table_need, &table_capacity) ||
      table_capacity > static_cast<size_t>(std::numeric_limits<uint32_t>::max())) {
    return nullptr;
  }

  uint32_t* d_slot_owners = sycl::malloc_shared<uint32_t>(table_capacity, *q);
  uint32_t* d_slot_counts = sycl::malloc_shared<uint32_t>(table_capacity, *q);
  uint32_t* d_slot_present = sycl::malloc_shared<uint32_t>(table_capacity, *q);
  uint32_t* d_slot_offsets = sycl::malloc_shared<uint32_t>(table_capacity, *q);
  int64_t* d_out_keys = sycl::malloc_shared<int64_t>(row_count, *q);
  int64_t* d_out_counts = sycl::malloc_shared<int64_t>(row_count, *q);
  double* d_out_results = sycl::malloc_shared<double>(row_count, *q);
  uint32_t* d_group_count = sycl::malloc_shared<uint32_t>(1, *q);
  uint32_t* d_overflow = sycl::malloc_shared<uint32_t>(1, *q);

  auto cleanup = [&]() {
    if (d_slot_owners)
      sycl::free(d_slot_owners, *q);
    if (d_slot_counts)
      sycl::free(d_slot_counts, *q);
    if (d_slot_present)
      sycl::free(d_slot_present, *q);
    if (d_slot_offsets)
      sycl::free(d_slot_offsets, *q);
    if (d_out_keys)
      sycl::free(d_out_keys, *q);
    if (d_out_counts)
      sycl::free(d_out_counts, *q);
    if (d_out_results)
      sycl::free(d_out_results, *q);
    if (d_group_count)
      sycl::free(d_group_count, *q);
    if (d_overflow)
      sycl::free(d_overflow, *q);
  };

  if (d_slot_owners == nullptr || d_slot_counts == nullptr || d_slot_present == nullptr ||
      d_slot_offsets == nullptr || d_out_keys == nullptr || d_out_counts == nullptr ||
      d_out_results == nullptr || d_group_count == nullptr || d_overflow == nullptr) {
    cleanup();
    return nullptr;
  }

  try {
    q->fill(d_slot_owners, HASH_AGG_GROUP_NONE, table_capacity).wait_and_throw();
    q->fill(d_slot_counts, 0u, table_capacity).wait_and_throw();
    q->fill(d_overflow, 0u, 1).wait_and_throw();
  } catch (const std::exception& e) {
    std::fprintf(stderr, "pgaccel: hash_count_i64 device-hash init failed: %s\n", e.what());
    cleanup();
    return nullptr;
  } catch (...) {
    std::fprintf(stderr, "pgaccel: hash_count_i64 device-hash init failed (unknown)\n");
    cleanup();
    return nullptr;
  }

  const uint32_t mask = static_cast<uint32_t>(table_capacity - 1);

  try {
    q->parallel_for(sycl::range<1>(row_count), [=](sycl::id<1> id) {
       const uint32_t r = static_cast<uint32_t>(id[0]);
       const int64_t key = group_keys[r];
       uint32_t slot = static_cast<uint32_t>(hash64(static_cast<uint64_t>(key))) & mask;

       for (uint32_t probe = 0; probe <= mask; ++probe) {
         sycl::atomic_ref<uint32_t, sycl::memory_order::relaxed, sycl::memory_scope::device,
                          sycl::access::address_space::global_space>
             owner_ref(d_slot_owners[slot]);
         uint32_t owner = owner_ref.load();

         if (owner == HASH_AGG_GROUP_NONE) {
           uint32_t expected = HASH_AGG_GROUP_NONE;
           if (owner_ref.compare_exchange_strong(expected, r)) {
             sycl::atomic_ref<uint32_t, sycl::memory_order::relaxed, sycl::memory_scope::device,
                              sycl::access::address_space::global_space>
                 count_ref(d_slot_counts[slot]);
             count_ref.fetch_add(1u);
             return;
           }
           owner = expected;
         }

         if (owner != HASH_AGG_GROUP_NONE && group_keys[owner] == key) {
           sycl::atomic_ref<uint32_t, sycl::memory_order::relaxed, sycl::memory_scope::device,
                            sycl::access::address_space::global_space>
               count_ref(d_slot_counts[slot]);
           count_ref.fetch_add(1u);
           return;
         }

         slot = (slot + 1u) & mask;
       }

       sycl::atomic_ref<uint32_t, sycl::memory_order::relaxed, sycl::memory_scope::device,
                        sycl::access::address_space::global_space>
           overflow_ref(d_overflow[0]);
       overflow_ref.store(1u);
     }).wait_and_throw();
    pgaccel_record_gpu_exec();
  } catch (const std::exception& e) {
    std::fprintf(stderr, "pgaccel: hash_count_i64 device-hash build kernel failed: %s\n", e.what());
    cleanup();
    return nullptr;
  } catch (...) {
    std::fprintf(stderr, "pgaccel: hash_count_i64 device-hash build kernel failed (unknown)\n");
    cleanup();
    return nullptr;
  }

  if (*d_overflow != 0) {
    cleanup();
    return nullptr;
  }

  try {
    q->parallel_for(sycl::range<1>(table_capacity), [=](sycl::id<1> id) {
       const uint32_t slot = static_cast<uint32_t>(id[0]);
       const uint32_t owner = d_slot_owners[slot];
       d_slot_present[slot] =
           owner != HASH_AGG_GROUP_NONE && d_slot_counts[slot] != 0 ? 1u : 0u;
     }).wait_and_throw();
  } catch (const std::exception& e) {
    std::fprintf(stderr, "pgaccel: hash_count_i64 device-hash occupancy kernel failed: %s\n",
                 e.what());
    cleanup();
    return nullptr;
  } catch (...) {
    std::fprintf(stderr, "pgaccel: hash_count_i64 device-hash occupancy kernel failed (unknown)\n");
    cleanup();
    return nullptr;
  }

  if (!device_exclusive_scan_u32(*q, d_slot_present, table_capacity, d_slot_offsets,
                                 d_group_count)) {
    cleanup();
    return nullptr;
  }

  const uint32_t n_groups = *d_group_count;
  if (n_groups == 0 || static_cast<size_t>(n_groups) > max_distinct ||
      static_cast<size_t>(n_groups) > row_count) {
    cleanup();
    return nullptr;
  }

  try {
    q->parallel_for(sycl::range<1>(table_capacity), [=](sycl::id<1> id) {
       const uint32_t slot = static_cast<uint32_t>(id[0]);
       if (d_slot_present[slot] == 0)
         return;
       const uint32_t group_id = d_slot_offsets[slot];
       const uint32_t owner = d_slot_owners[slot];
       d_out_keys[group_id] = group_keys[owner];
     }).wait_and_throw();
    pgaccel_record_gpu_exec();
  } catch (const std::exception& e) {
    std::fprintf(stderr, "pgaccel: hash_count_i64 device-hash compact kernel failed: %s\n",
                 e.what());
    cleanup();
    return nullptr;
  } catch (...) {
    std::fprintf(stderr, "pgaccel: hash_count_i64 device-hash compact kernel failed (unknown)\n");
    cleanup();
    return nullptr;
  }

  // Hash-table placement depends on which colliding key wins a slot first.
  // Sort the compacted keys, then look each key back up in the resident table
  // so output order is deterministic without moving counts on the host.
  if (pgaccel_sort_i64(d_out_keys, n_groups) != PGACCEL_OK) {
    cleanup();
    return nullptr;
  }

  try {
    q->fill(d_overflow, 0u, 1).wait_and_throw();
    q->parallel_for(sycl::range<1>(n_groups), [=](sycl::id<1> id) {
       const uint32_t group_id = static_cast<uint32_t>(id[0]);
       const int64_t key = d_out_keys[group_id];
       uint32_t slot = static_cast<uint32_t>(hash64(static_cast<uint64_t>(key))) & mask;

       for (uint32_t probe = 0; probe <= mask; ++probe) {
         const uint32_t owner = d_slot_owners[slot];
         if (owner != HASH_AGG_GROUP_NONE && group_keys[owner] == key) {
           const int64_t count = static_cast<int64_t>(d_slot_counts[slot]);
           d_out_counts[group_id] = count;
           d_out_results[group_id] = static_cast<double>(count);
           return;
         }
         slot = (slot + 1u) & mask;
       }

       sycl::atomic_ref<uint32_t, sycl::memory_order::relaxed, sycl::memory_scope::device,
                        sycl::access::address_space::global_space>
           failure_ref(d_overflow[0]);
       failure_ref.store(1u);
     }).wait_and_throw();
    pgaccel_record_gpu_exec();
  } catch (const std::exception& e) {
    std::fprintf(stderr, "pgaccel: hash_count_i64 device-hash finalize kernel failed: %s\n",
                 e.what());
    cleanup();
    return nullptr;
  } catch (...) {
    std::fprintf(stderr, "pgaccel: hash_count_i64 device-hash finalize kernel failed (unknown)\n");
    cleanup();
    return nullptr;
  }

  if (*d_overflow != 0) {
    cleanup();
    return nullptr;
  }

  auto* state = new (std::nothrow) pgaccel_agg_state();
  if (state == nullptr) {
    cleanup();
    return nullptr;
  }
  try {
    const size_t key_bytes = static_cast<size_t>(n_groups) * sizeof(int64_t);
    const size_t result_bytes = static_cast<size_t>(n_groups) * sizeof(double);
    state->key_size = sizeof(int64_t);
    state->key_type = 1;
    state->group_count = n_groups;
    state->num_aggs = 1;
    state->group_key_buf.resize(key_bytes);
    state->counts.resize(n_groups);
    state->results.resize(1);
    state->results[0].resize(n_groups);
    q->memcpy(state->group_key_buf.data(), d_out_keys, key_bytes).wait_and_throw();
    q->memcpy(state->counts.data(), d_out_counts, key_bytes).wait_and_throw();
    q->memcpy(state->results[0].data(), d_out_results, result_bytes).wait_and_throw();
  } catch (...) {
    delete state;
    cleanup();
    throw;
  }
  cleanup();
  return state;
}

static pgaccel_agg_state* agg_count_i64_sorted_device(int64_t* group_keys, size_t row_count) {
  sycl::queue* q = pgaccel_get_queue();
  if (q == nullptr || group_keys == nullptr || row_count == 0 ||
      row_count > static_cast<size_t>(std::numeric_limits<uint32_t>::max())) {
    return nullptr;
  }

  size_t out_key_bytes = 0;
  size_t out_count_bytes = 0;
  size_t out_result_bytes = 0;
  if (!checked_mul_size(row_count, sizeof(int64_t), &out_key_bytes) ||
      !checked_mul_size(row_count, sizeof(int64_t), &out_count_bytes) ||
      !checked_mul_size(row_count, sizeof(double), &out_result_bytes)) {
    return nullptr;
  }

  static constexpr size_t COUNT_COMPACT_BLOCK_ROWS = 256;
  const size_t num_blocks = (row_count + COUNT_COMPACT_BLOCK_ROWS - 1) / COUNT_COMPACT_BLOCK_ROWS;
  size_t block_bytes = 0;
  if (!checked_mul_size(num_blocks, sizeof(uint32_t), &block_bytes))
    return nullptr;

  const size_t counts_off = out_key_bytes;
  size_t results_off = 0;
  size_t block_counts_off = 0;
  size_t block_offsets_off = 0;
  size_t group_count_off = 0;
  size_t slab_bytes = 0;
  if (!checked_add_size(counts_off, out_count_bytes, &results_off) ||
      !checked_add_size(results_off, out_result_bytes, &block_counts_off) ||
      !checked_add_size(block_counts_off, block_bytes, &block_offsets_off) ||
      !checked_add_size(block_offsets_off, block_bytes, &group_count_off) ||
      !checked_add_size(group_count_off, sizeof(uint32_t), &slab_bytes)) {
    return nullptr;
  }

  uint8_t* d_slab = sycl::malloc_shared<uint8_t>(slab_bytes, *q);
  if (d_slab == nullptr)
    return nullptr;

  auto cleanup = [&]() {
    if (d_slab)
      sycl::free(d_slab, *q);
  };

  auto* block_counts = reinterpret_cast<uint32_t*>(d_slab + block_counts_off);
  auto* block_offsets = reinterpret_cast<uint32_t*>(d_slab + block_offsets_off);
  auto* group_count = reinterpret_cast<uint32_t*>(d_slab + group_count_off);

  try {
    uint32_t* counts_by_block = block_counts;
    const size_t rows = row_count;
    auto nd = sycl::nd_range<1>(sycl::range<1>(num_blocks * COUNT_COMPACT_BLOCK_ROWS),
                                sycl::range<1>(COUNT_COMPACT_BLOCK_ROWS));
    q->submit([&](sycl::handler& h) {
       sycl::local_accessor<uint32_t, 1> starts(sycl::range<1>(COUNT_COMPACT_BLOCK_ROWS), h);
       h.parallel_for(nd, [=](sycl::nd_item<1> it) {
         const size_t lid = it.get_local_id(0);
         const size_t block = it.get_group(0);
         const size_t i = block * COUNT_COMPACT_BLOCK_ROWS + lid;

         uint32_t is_start = 0;
         if (i < rows && (i == 0 || group_keys[i - 1] != group_keys[i]))
           is_start = 1;
         starts[lid] = is_start;
         sycl::group_barrier(it.get_group());

         for (size_t stride = COUNT_COMPACT_BLOCK_ROWS / 2; stride > 0; stride >>= 1) {
           if (lid < stride)
             starts[lid] += starts[lid + stride];
           sycl::group_barrier(it.get_group());
         }

         if (lid == 0)
           counts_by_block[block] = starts[0];
       });
     }).wait_and_throw();
    pgaccel_record_gpu_exec();
  } catch (const std::exception& e) {
    std::fprintf(stderr, "pgaccel: hash_count_i64 sort-reduce count kernel failed: %s\n", e.what());
    cleanup();
    return nullptr;
  } catch (...) {
    std::fprintf(stderr, "pgaccel: hash_count_i64 sort-reduce count kernel failed (unknown)\n");
    cleanup();
    return nullptr;
  }

  if (!device_exclusive_scan_u32(*q, block_counts, num_blocks, block_offsets, group_count)) {
    cleanup();
    return nullptr;
  }

  const uint32_t n_groups = *group_count;
  if (n_groups == 0 || static_cast<size_t>(n_groups) > row_count) {
    cleanup();
    return nullptr;
  }

  try {
    auto* out_keys = reinterpret_cast<int64_t*>(d_slab);
    auto* out_counts = reinterpret_cast<int64_t*>(d_slab + counts_off);
    auto* out_results = reinterpret_cast<double*>(d_slab + results_off);
    uint32_t* offsets_by_block = block_offsets;
    const size_t rows = row_count;
    auto nd = sycl::nd_range<1>(sycl::range<1>(num_blocks * COUNT_COMPACT_BLOCK_ROWS),
                                sycl::range<1>(COUNT_COMPACT_BLOCK_ROWS));
    q->submit([&](sycl::handler& h) {
       sycl::local_accessor<uint32_t, 1> starts(sycl::range<1>(COUNT_COMPACT_BLOCK_ROWS), h);
       h.parallel_for(nd, [=](sycl::nd_item<1> it) {
         const size_t lid = it.get_local_id(0);
         const size_t block = it.get_group(0);
         const size_t i = block * COUNT_COMPACT_BLOCK_ROWS + lid;

         uint32_t is_start = 0;
         if (i < rows && (i == 0 || group_keys[i - 1] != group_keys[i]))
           is_start = 1;
         starts[lid] = is_start;
         sycl::group_barrier(it.get_group());

         if (is_start == 0)
           return;

         uint32_t local_group = 0;
         for (size_t j = 0; j < lid; ++j)
           local_group += starts[j];

         const int64_t key = group_keys[i];
         int64_t count = 1;
         size_t j = i + 1;
         while (j < rows && group_keys[j] == key) {
           ++count;
           ++j;
         }

         const uint32_t out_idx = offsets_by_block[block] + local_group;
         out_keys[out_idx] = key;
         out_counts[out_idx] = count;
         out_results[out_idx] = static_cast<double>(count);
       });
     }).wait_and_throw();
    pgaccel_record_gpu_exec();
  } catch (const std::exception& e) {
    std::fprintf(stderr, "pgaccel: hash_count_i64 sort-reduce emit kernel failed: %s\n", e.what());
    cleanup();
    return nullptr;
  } catch (...) {
    std::fprintf(stderr, "pgaccel: hash_count_i64 sort-reduce emit kernel failed (unknown)\n");
    cleanup();
    return nullptr;
  }

  auto* state = new (std::nothrow) pgaccel_agg_state();
  if (state == nullptr) {
    cleanup();
    return nullptr;
  }
  try {
    const size_t key_bytes = static_cast<size_t>(n_groups) * sizeof(int64_t);
    const size_t result_bytes = static_cast<size_t>(n_groups) * sizeof(double);
    state->key_size = sizeof(int64_t);
    state->key_type = 1;
    state->group_count = n_groups;
    state->num_aggs = 1;
    state->group_key_buf.resize(key_bytes);
    state->counts.resize(n_groups);
    state->results.resize(1);
    state->results[0].resize(n_groups);
    q->memcpy(state->group_key_buf.data(), d_slab, key_bytes).wait_and_throw();
    q->memcpy(state->counts.data(), d_slab + counts_off, key_bytes).wait_and_throw();
    q->memcpy(state->results[0].data(), d_slab + results_off, result_bytes).wait_and_throw();
  } catch (...) {
    delete state;
    cleanup();
    throw;
  }
  cleanup();
  return state;
}

static pgaccel_agg_state* agg_count_i64_sort_reduce_device(int64_t* group_keys, size_t row_count) {
  if (group_keys == nullptr || row_count == 0 ||
      row_count > static_cast<size_t>(std::numeric_limits<uint32_t>::max())) {
    return nullptr;
  }

  const pgaccel_status sort_status =
      pgaccel_sort_u64(reinterpret_cast<uint64_t*>(group_keys), row_count);
  if (sort_status != PGACCEL_OK)
    return nullptr;

  return agg_count_i64_sorted_device(group_keys, row_count);
}

}  // namespace

// ---------------------------------------------------------------------------
// Sort-based grouped aggregation (GPU sort + SYCL per-group reduce).
// ---------------------------------------------------------------------------

static pgaccel_agg_state* agg_sort_based(const void* group_keys, const uint8_t* group_null_mask,
                                         size_t row_count, int key_type,
                                         const void* const* value_cols,
                                         const uint8_t* const* value_nulls, const int* value_types,
                                         const pgaccel_agg_col* agg_cols, size_t num_aggs) {
  sycl::queue* q = pgaccel_get_queue();
  if (q == nullptr)
    return nullptr;

  size_t ksz = key_size_for_type(key_type);
  if (ksz == 0)
    return nullptr;
  if (row_count > static_cast<size_t>(std::numeric_limits<uint32_t>::max()))
    return nullptr;
  if (null_sort_sentinel_collides(group_keys, group_null_mask, row_count, key_type))
    return nullptr;

  // -- Phase 1: GPU sort by key. NULLs get a sentinel that sorts to
  // the end so we can detect them as the last group.
  uint32_t* indices = sycl::malloc_shared<uint32_t>(row_count, *q);
  if (indices == nullptr)
    return nullptr;
  for (size_t i = 0; i < row_count; ++i)
    indices[i] = static_cast<uint32_t>(i);

  pgaccel_status st = PGACCEL_ERROR;
  std::vector<int32_t> keys_i32;
  std::vector<int64_t> keys_i64;
  std::vector<double> keys_f64;

  switch (key_type) {
    case 0: {
      keys_i32.resize(row_count);
      auto* src = static_cast<const int32_t*>(group_keys);
      for (size_t i = 0; i < row_count; ++i) {
        keys_i32[i] = (group_null_mask != nullptr && group_null_mask[i])
                          ? std::numeric_limits<int32_t>::max()
                          : src[i];
      }
      st = pgaccel_sort_kv_i32(keys_i32.data(), indices, row_count);
      break;
    }
    case 1: {
      keys_i64.resize(row_count);
      auto* src = static_cast<const int64_t*>(group_keys);
      for (size_t i = 0; i < row_count; ++i) {
        keys_i64[i] = (group_null_mask != nullptr && group_null_mask[i])
                          ? std::numeric_limits<int64_t>::max()
                          : src[i];
      }
      st = pgaccel_sort_kv_i64(keys_i64.data(), indices, row_count);
      break;
    }
    case 2: {
      keys_f64.resize(row_count);
      auto* src = static_cast<const double*>(group_keys);
      // Sentinel sorts NULL keys to the end. The file is on the
      // -fno-fast-math opt-out list (pgaccel-kernels/CMakeLists.txt) so
      // +infinity as a sort sentinel is well-defined.
      const double inf_sentinel = std::numeric_limits<double>::infinity();
      for (size_t i = 0; i < row_count; ++i) {
        keys_f64[i] = (group_null_mask != nullptr && group_null_mask[i]) ? inf_sentinel : src[i];
      }
      st = pgaccel_sort_kv_f64(keys_f64.data(), indices, row_count);
      break;
    }
    default:
      sycl::free(indices, *q);
      return nullptr;
  }

  if (st != PGACCEL_OK) {
    sycl::free(indices, *q);
    return nullptr;
  }

  // -- Phase 2 (host orchestration): scan sorted keys to find group
  // boundaries. This is O(n) sequential but cheap relative to the sort.
  std::vector<size_t> group_start_vec;
  group_start_vec.push_back(0);
  for (size_t i = 1; i < row_count; ++i) {
    bool different = false;
    switch (key_type) {
      case 0:
        different = (keys_i32[i] != keys_i32[i - 1]);
        break;
      case 1:
        different = (keys_i64[i] != keys_i64[i - 1]);
        break;
      case 2: {
        double a = keys_f64[i], b = keys_f64[i - 1];
        bool a_nan = (a != a), b_nan = (b != b);
        different = !((a_nan && b_nan) || a == b);
        break;
      }
    }
    if (different)
      group_start_vec.push_back(i);
  }

  size_t n_groups = group_start_vec.size();

  // Stage group-bounds + value-cols into shared memory for the kernel.
  size_t* d_group_starts = sycl::malloc_shared<size_t>(n_groups, *q);
  size_t* d_group_ends = sycl::malloc_shared<size_t>(n_groups, *q);
  if (d_group_starts == nullptr || d_group_ends == nullptr) {
    if (d_group_starts)
      sycl::free(d_group_starts, *q);
    if (d_group_ends)
      sycl::free(d_group_ends, *q);
    sycl::free(indices, *q);
    return nullptr;
  }
  for (size_t g = 0; g < n_groups; ++g) {
    d_group_starts[g] = group_start_vec[g];
    d_group_ends[g] = (g + 1 < n_groups) ? group_start_vec[g + 1] : row_count;
  }

  // Detect the NULL sentinel group (sorts to the end).
  bool has_null_group = false;
  if (group_null_mask != nullptr) {
    uint32_t last_orig = indices[d_group_starts[n_groups - 1]];
    has_null_group = group_null_mask[last_orig] != 0;
  }

  // -- Phase 3 (SYCL kernel): per-group accumulation. Stage value cols.
  StagedColumnArrays* sc =
      stage_columns(*q, row_count, num_aggs, value_cols, value_nulls, value_types, agg_cols);
  if (sc == nullptr) {
    sycl::free(d_group_starts, *q);
    sycl::free(d_group_ends, *q);
    sycl::free(indices, *q);
    return nullptr;
  }

  size_t result_count = 0;
  if (!checked_mul_size(num_aggs, n_groups, &result_count)) {
    free_staged_columns(*q, sc, num_aggs);
    sycl::free(d_group_starts, *q);
    sycl::free(d_group_ends, *q);
    sycl::free(indices, *q);
    return nullptr;
  }

  double* d_results = sycl::malloc_shared<double>(result_count, *q);
  int64_t* d_counts = sycl::malloc_shared<int64_t>(n_groups, *q);
  if (d_results == nullptr || d_counts == nullptr) {
    if (d_results)
      sycl::free(d_results, *q);
    if (d_counts)
      sycl::free(d_counts, *q);
    free_staged_columns(*q, sc, num_aggs);
    sycl::free(d_group_starts, *q);
    sycl::free(d_group_ends, *q);
    sycl::free(indices, *q);
    return nullptr;
  }

  if (!run_sorted_accum_kernel(*q, n_groups, num_aggs, indices, d_group_starts, d_group_ends,
                               sc->d_value_data, sc->value_data_bytes, sc->d_null_data,
                               sc->null_data_bytes, sc->d_value_offsets, sc->d_null_offsets,
                               sc->d_null_present, sc->d_value_types, sc->d_funcs, sc->d_col_idx,
                               d_results, d_counts)) {
    sycl::free(d_results, *q);
    sycl::free(d_counts, *q);
    free_staged_columns(*q, sc, num_aggs);
    sycl::free(d_group_starts, *q);
    sycl::free(d_group_ends, *q);
    sycl::free(indices, *q);
    return nullptr;
  }

  // -- Phase 4: build the result state.
  auto* state = new (std::nothrow) pgaccel_agg_state();
  if (state == nullptr) {
    sycl::free(d_results, *q);
    sycl::free(d_counts, *q);
    free_staged_columns(*q, sc, num_aggs);
    sycl::free(d_group_starts, *q);
    sycl::free(d_group_ends, *q);
    sycl::free(indices, *q);
    return nullptr;
  }
  state->key_size = ksz;
  state->key_type = key_type;
  state->group_count = n_groups;
  state->num_aggs = num_aggs;

  // Build group key buffer (one key per group from first row).
  size_t group_key_bytes = 0;
  if (!checked_mul_size(n_groups, ksz, &group_key_bytes)) {
    sycl::free(d_results, *q);
    sycl::free(d_counts, *q);
    free_staged_columns(*q, sc, num_aggs);
    sycl::free(d_group_starts, *q);
    sycl::free(d_group_ends, *q);
    sycl::free(indices, *q);
    delete state;
    return nullptr;
  }
  state->group_key_buf.resize(group_key_bytes);
  for (size_t g = 0; g < n_groups; ++g) {
    uint32_t orig_row = indices[d_group_starts[g]];
    if (has_null_group && g == n_groups - 1) {
      memset(state->group_key_buf.data() + g * ksz, 0, ksz);
    } else {
      const uint8_t* src_key = static_cast<const uint8_t*>(group_keys) + orig_row * ksz;
      memcpy(state->group_key_buf.data() + g * ksz, src_key, ksz);
    }
  }

  state->counts.assign(d_counts, d_counts + n_groups);
  state->results.resize(num_aggs);
  for (size_t a = 0; a < num_aggs; ++a) {
    state->results[a].assign(d_results + a * n_groups, d_results + (a + 1) * n_groups);
  }

  sycl::free(d_results, *q);
  sycl::free(d_counts, *q);
  free_staged_columns(*q, sc, num_aggs);
  sycl::free(d_group_starts, *q);
  sycl::free(d_group_ends, *q);
  sycl::free(indices, *q);
  pgaccel_record_gpu_exec();
  return state;
}

// ===========================================================================
// PARTIAL-MODE host dispatch (Phase 3B).
//
// Mirrors agg_sort_based but calls the partial kernel and
// stores per-agg per-group transition states with widths matching
// pgaccel_agg_partial_width. Group assignment remains GPU-only.
// ===========================================================================

namespace {

// Compute total partial-output buffer size in doubles + per-agg offsets
// (in doubles, not bytes). Returns false on overflow.
bool compute_partial_layout(const pgaccel_agg_col* agg_cols, size_t num_aggs, size_t n_groups,
                            std::vector<size_t>& widths, std::vector<size_t>& offsets,
                            size_t& total) {
  widths.resize(num_aggs);
  offsets.resize(num_aggs);
  total = 0;
  for (size_t a = 0; a < num_aggs; ++a) {
    widths[a] = pgaccel_agg_partial_width(agg_cols[a].func);
    offsets[a] = total;
    if (widths[a] == 0)
      return false;
    size_t agg_group_width = 0;
    if (!checked_mul_size(widths[a], n_groups, &agg_group_width) ||
        !checked_add_size(total, agg_group_width, &total))
      return false;
  }
  return true;
}

}  // namespace

static pgaccel_agg_state* agg_sort_based_partial(const void* group_keys,
                                                 const uint8_t* group_null_mask, size_t row_count,
                                                 int key_type, const void* const* value_cols,
                                                 const uint8_t* const* value_nulls,
                                                 const int* value_types,
                                                 const pgaccel_agg_col* agg_cols, size_t num_aggs) {
  sycl::queue* q = pgaccel_get_queue();
  if (q == nullptr)
    return nullptr;

  size_t ksz = key_size_for_type(key_type);
  if (ksz == 0)
    return nullptr;
  if (row_count > static_cast<size_t>(std::numeric_limits<uint32_t>::max()))
    return nullptr;
  if (null_sort_sentinel_collides(group_keys, group_null_mask, row_count, key_type))
    return nullptr;

  uint32_t* indices = sycl::malloc_shared<uint32_t>(row_count, *q);
  if (indices == nullptr)
    return nullptr;
  for (size_t i = 0; i < row_count; ++i)
    indices[i] = static_cast<uint32_t>(i);

  pgaccel_status st = PGACCEL_ERROR;
  std::vector<int32_t> keys_i32;
  std::vector<int64_t> keys_i64;
  std::vector<double> keys_f64;
  switch (key_type) {
    case 0: {
      keys_i32.resize(row_count);
      auto* src = static_cast<const int32_t*>(group_keys);
      for (size_t i = 0; i < row_count; ++i) {
        keys_i32[i] = (group_null_mask != nullptr && group_null_mask[i])
                          ? std::numeric_limits<int32_t>::max()
                          : src[i];
      }
      st = pgaccel_sort_kv_i32(keys_i32.data(), indices, row_count);
      break;
    }
    case 1: {
      keys_i64.resize(row_count);
      auto* src = static_cast<const int64_t*>(group_keys);
      for (size_t i = 0; i < row_count; ++i) {
        keys_i64[i] = (group_null_mask != nullptr && group_null_mask[i])
                          ? std::numeric_limits<int64_t>::max()
                          : src[i];
      }
      st = pgaccel_sort_kv_i64(keys_i64.data(), indices, row_count);
      break;
    }
    case 2: {
      keys_f64.resize(row_count);
      auto* src = static_cast<const double*>(group_keys);
      const double inf_sentinel = std::numeric_limits<double>::infinity();
      for (size_t i = 0; i < row_count; ++i) {
        keys_f64[i] = (group_null_mask != nullptr && group_null_mask[i]) ? inf_sentinel : src[i];
      }
      st = pgaccel_sort_kv_f64(keys_f64.data(), indices, row_count);
      break;
    }
    default:
      sycl::free(indices, *q);
      return nullptr;
  }

  if (st != PGACCEL_OK) {
    sycl::free(indices, *q);
    return nullptr;
  }

  std::vector<size_t> group_start_vec;
  group_start_vec.push_back(0);
  for (size_t i = 1; i < row_count; ++i) {
    bool different = false;
    switch (key_type) {
      case 0:
        different = (keys_i32[i] != keys_i32[i - 1]);
        break;
      case 1:
        different = (keys_i64[i] != keys_i64[i - 1]);
        break;
      case 2: {
        double a = keys_f64[i], b = keys_f64[i - 1];
        bool a_nan = (a != a), b_nan = (b != b);
        different = !((a_nan && b_nan) || a == b);
        break;
      }
    }
    if (different)
      group_start_vec.push_back(i);
  }

  size_t n_groups = group_start_vec.size();

  size_t* d_group_starts = sycl::malloc_shared<size_t>(n_groups, *q);
  size_t* d_group_ends = sycl::malloc_shared<size_t>(n_groups, *q);
  if (d_group_starts == nullptr || d_group_ends == nullptr) {
    if (d_group_starts)
      sycl::free(d_group_starts, *q);
    if (d_group_ends)
      sycl::free(d_group_ends, *q);
    sycl::free(indices, *q);
    return nullptr;
  }
  for (size_t g = 0; g < n_groups; ++g) {
    d_group_starts[g] = group_start_vec[g];
    d_group_ends[g] = (g + 1 < n_groups) ? group_start_vec[g + 1] : row_count;
  }

  bool has_null_group = false;
  if (group_null_mask != nullptr) {
    uint32_t last_orig = indices[d_group_starts[n_groups - 1]];
    has_null_group = group_null_mask[last_orig] != 0;
  }

  StagedColumnArrays* sc =
      stage_columns(*q, row_count, num_aggs, value_cols, value_nulls, value_types, agg_cols);
  if (sc == nullptr) {
    sycl::free(d_group_starts, *q);
    sycl::free(d_group_ends, *q);
    sycl::free(indices, *q);
    return nullptr;
  }

  std::vector<size_t> widths_h, offsets_h;
  size_t total = 0;
  if (!compute_partial_layout(agg_cols, num_aggs, n_groups, widths_h, offsets_h, total)) {
    free_staged_columns(*q, sc, num_aggs);
    sycl::free(d_group_starts, *q);
    sycl::free(d_group_ends, *q);
    sycl::free(indices, *q);
    return nullptr;
  }

  size_t* d_widths = sycl::malloc_shared<size_t>(num_aggs, *q);
  size_t* d_offsets = sycl::malloc_shared<size_t>(num_aggs, *q);
  double* d_partials = sycl::malloc_shared<double>(total > 0 ? total : 1, *q);
  int64_t* d_counts = sycl::malloc_shared<int64_t>(n_groups, *q);
  if (d_widths == nullptr || d_offsets == nullptr || d_partials == nullptr || d_counts == nullptr) {
    if (d_widths)
      sycl::free(d_widths, *q);
    if (d_offsets)
      sycl::free(d_offsets, *q);
    if (d_partials)
      sycl::free(d_partials, *q);
    if (d_counts)
      sycl::free(d_counts, *q);
    free_staged_columns(*q, sc, num_aggs);
    sycl::free(d_group_starts, *q);
    sycl::free(d_group_ends, *q);
    sycl::free(indices, *q);
    return nullptr;
  }
  for (size_t a = 0; a < num_aggs; ++a) {
    d_widths[a] = widths_h[a];
    d_offsets[a] = offsets_h[a];
  }

  if (!run_sorted_partial_kernel(*q, n_groups, num_aggs, indices, d_group_starts, d_group_ends,
                                 sc->d_value_data, sc->value_data_bytes, sc->d_null_data,
                                 sc->null_data_bytes, sc->d_value_offsets, sc->d_null_offsets,
                                 sc->d_null_present, sc->d_value_types, sc->d_funcs, sc->d_col_idx,
                                 d_offsets, d_widths, d_partials, d_counts, total)) {
    sycl::free(d_widths, *q);
    sycl::free(d_offsets, *q);
    sycl::free(d_partials, *q);
    sycl::free(d_counts, *q);
    free_staged_columns(*q, sc, num_aggs);
    sycl::free(d_group_starts, *q);
    sycl::free(d_group_ends, *q);
    sycl::free(indices, *q);
    return nullptr;
  }

  auto* state = new (std::nothrow) pgaccel_agg_state();
  if (state == nullptr) {
    sycl::free(d_widths, *q);
    sycl::free(d_offsets, *q);
    sycl::free(d_partials, *q);
    sycl::free(d_counts, *q);
    free_staged_columns(*q, sc, num_aggs);
    sycl::free(d_group_starts, *q);
    sycl::free(d_group_ends, *q);
    sycl::free(indices, *q);
    return nullptr;
  }
  state->key_size = ksz;
  state->key_type = key_type;
  state->group_count = n_groups;
  state->num_aggs = num_aggs;

  size_t group_key_bytes = 0;
  if (!checked_mul_size(n_groups, ksz, &group_key_bytes)) {
    sycl::free(d_widths, *q);
    sycl::free(d_offsets, *q);
    sycl::free(d_partials, *q);
    sycl::free(d_counts, *q);
    free_staged_columns(*q, sc, num_aggs);
    sycl::free(d_group_starts, *q);
    sycl::free(d_group_ends, *q);
    sycl::free(indices, *q);
    delete state;
    return nullptr;
  }
  state->group_key_buf.resize(group_key_bytes);
  for (size_t g = 0; g < n_groups; ++g) {
    uint32_t orig_row = indices[d_group_starts[g]];
    if (has_null_group && g == n_groups - 1) {
      memset(state->group_key_buf.data() + g * ksz, 0, ksz);
    } else {
      const uint8_t* src_key = static_cast<const uint8_t*>(group_keys) + orig_row * ksz;
      memcpy(state->group_key_buf.data() + g * ksz, src_key, ksz);
    }
  }

  state->counts.assign(d_counts, d_counts + n_groups);
  state->partial_widths = std::move(widths_h);
  state->partial_results.resize(num_aggs);
  for (size_t a = 0; a < num_aggs; ++a) {
    const size_t off = offsets_h[a];
    const size_t len = state->partial_widths[a] * n_groups;
    state->partial_results[a].assign(d_partials + off, d_partials + off + len);
  }

  sycl::free(d_widths, *q);
  sycl::free(d_offsets, *q);
  sycl::free(d_partials, *q);
  sycl::free(d_counts, *q);
  free_staged_columns(*q, sc, num_aggs);
  sycl::free(d_group_starts, *q);
  sycl::free(d_group_ends, *q);
  sycl::free(indices, *q);
  pgaccel_record_gpu_exec();
  return state;
}

namespace {

// The standalone checked ABI receives host columns, so it cannot retain the
// resident engine's zero-copy property. Keep staging bounded and retain the
// hash table plus aggregate state across chunks in one shared USM allocation.
// Group lookup, collision resolution, aggregate updates, and cross-chunk
// merging all happen in the kernels below.
static constexpr size_t HASH_AGG_STREAM_MAX_TABLE_SLOTS = size_t{1} << 20;
static constexpr size_t HASH_AGG_STREAM_MAX_SLAB_BYTES = size_t{256} << 20;
static constexpr size_t HASH_AGG_STREAM_BLOCK_ROWS = 256;
static constexpr size_t HASH_AGG_STREAM_BLOCK_TABLE_SLOTS = HASH_AGG_STREAM_BLOCK_ROWS * 2;
static constexpr uint32_t HASH_AGG_STREAM_NO_GROUP = std::numeric_limits<uint32_t>::max();
static constexpr uint32_t HASH_AGG_STREAM_OVERFLOW_GROUPS = 1;
static constexpr uint32_t HASH_AGG_STREAM_OVERFLOW_TABLE = 2;
static constexpr uint32_t HASH_AGG_STREAM_OVERFLOW_COUNT = 3;
static constexpr const char* HASH_AGG_STREAM_TEST_CHUNK_ENV = "PGACCEL_TEST_HASHAGG_CHUNK_ROWS";

struct HashAggStreamingSlabHeader {
  size_t table_capacity;
  size_t max_groups;
  size_t num_aggs;
  size_t chunk_capacity;
  size_t chunk_rows;
  size_t chunk_blocks;
  size_t slot_used_off;
  size_t slot_bits_off;
  size_t slot_groups_off;
  size_t group_keys_off;
  size_t group_counts_off;
  size_t results_off;
  size_t value_offsets_off;
  size_t null_offsets_off;
  size_t null_present_off;
  size_t value_types_off;
  size_t agg_funcs_off;
  size_t agg_col_idx_off;
  size_t chunk_keys_off;
  size_t chunk_key_nulls_off;
  size_t chunk_value_data_off;
  size_t chunk_null_data_off;
  size_t block_slot_used_off;
  size_t block_slot_bits_off;
  size_t block_slot_groups_off;
  size_t block_group_counts_off;
  size_t partial_keys_off;
  size_t partial_key_nulls_off;
  size_t partial_counts_off;
  size_t partial_results_off;
  uint32_t group_count;
  uint32_t null_group;
  uint32_t overflow;
};

struct HashAggStreamingLayoutCursor {
  size_t cursor = sizeof(HashAggStreamingSlabHeader);
  bool valid = true;

  size_t append(size_t count, size_t elem_size, size_t alignment) {
    size_t aligned = 0;
    size_t span = 0;
    size_t next = 0;
    if (!valid || !checked_align_up(cursor, alignment, &aligned) ||
        !checked_mul_size(count, elem_size, &span) || !checked_add_size(aligned, span, &next)) {
      valid = false;
      return 0;
    }
    cursor = next;
    return aligned;
  }
};

bool build_hashagg_streaming_layout(size_t table_capacity, size_t num_aggs, size_t chunk_rows,
                                    size_t key_size, const uint8_t* const* value_nulls,
                                    const int* value_types, const pgaccel_agg_col* agg_cols,
                                    HashAggStreamingSlabHeader* out, size_t* out_bytes) {
  if (table_capacity < 2 || (table_capacity & (table_capacity - 1)) != 0 || out == nullptr ||
      out_bytes == nullptr)
    return false;

  HashAggStreamingSlabHeader h{};
  h.table_capacity = table_capacity;
  h.max_groups = table_capacity / 2;
  h.num_aggs = num_aggs;
  h.chunk_capacity = chunk_rows;
  h.chunk_rows = 0;
  h.chunk_blocks = 0;
  h.null_group = HASH_AGG_STREAM_NO_GROUP;

  HashAggStreamingLayoutCursor layout;
  size_t result_count = 0;
  if (!checked_mul_size(num_aggs, h.max_groups, &result_count))
    return false;
  h.slot_used_off = layout.append(table_capacity, sizeof(uint8_t), alignof(uint8_t));
  h.slot_bits_off = layout.append(table_capacity, sizeof(uint64_t), alignof(uint64_t));
  h.slot_groups_off = layout.append(table_capacity, sizeof(uint32_t), alignof(uint32_t));
  h.group_keys_off = layout.append(h.max_groups, key_size, key_size);
  h.group_counts_off = layout.append(h.max_groups, sizeof(int64_t), alignof(int64_t));
  h.results_off = layout.append(result_count, sizeof(double), alignof(double));
  h.value_offsets_off = layout.append(num_aggs, sizeof(size_t), alignof(size_t));
  h.null_offsets_off = layout.append(num_aggs, sizeof(size_t), alignof(size_t));
  h.null_present_off = layout.append(num_aggs, sizeof(uint8_t), alignof(uint8_t));
  h.value_types_off = layout.append(num_aggs, sizeof(int), alignof(int));
  h.agg_funcs_off = layout.append(num_aggs, sizeof(pgaccel_agg_func), alignof(pgaccel_agg_func));
  h.agg_col_idx_off = layout.append(num_aggs, sizeof(size_t), alignof(size_t));
  h.chunk_keys_off = layout.append(chunk_rows, key_size, key_size);
  h.chunk_key_nulls_off = layout.append(chunk_rows, sizeof(uint8_t), alignof(uint8_t));

  h.chunk_value_data_off = layout.cursor;
  for (size_t a = 0; a < num_aggs; ++a) {
    if (agg_reads_value(agg_cols[a].func, agg_cols[a].col_idx)) {
      layout.append(chunk_rows, value_elem_size(value_types[a]), value_elem_size(value_types[a]));
    }
  }
  h.chunk_null_data_off = layout.cursor;
  for (size_t a = 0; a < num_aggs; ++a) {
    if (agg_reads_value(agg_cols[a].func, agg_cols[a].col_idx) && value_nulls != nullptr &&
        value_nulls[a] != nullptr) {
      layout.append(chunk_rows, sizeof(uint8_t), alignof(uint8_t));
    }
  }

  size_t chunk_blocks = 0;
  size_t block_slot_count = 0;
  size_t partial_result_count = 0;
  if (chunk_rows != 0) {
    chunk_blocks = 1 + (chunk_rows - 1) / HASH_AGG_STREAM_BLOCK_ROWS;
  }
  if (!checked_mul_size(chunk_blocks, HASH_AGG_STREAM_BLOCK_TABLE_SLOTS, &block_slot_count) ||
      !checked_mul_size(num_aggs, chunk_rows, &partial_result_count)) {
    return false;
  }
  h.block_slot_used_off = layout.append(block_slot_count, sizeof(uint8_t), alignof(uint8_t));
  h.block_slot_bits_off = layout.append(block_slot_count, sizeof(uint64_t), alignof(uint64_t));
  h.block_slot_groups_off = layout.append(block_slot_count, sizeof(uint32_t), alignof(uint32_t));
  h.block_group_counts_off = layout.append(chunk_blocks, sizeof(uint32_t), alignof(uint32_t));
  h.partial_keys_off = layout.append(chunk_rows, key_size, key_size);
  h.partial_key_nulls_off = layout.append(chunk_rows, sizeof(uint8_t), alignof(uint8_t));
  h.partial_counts_off = layout.append(chunk_rows, sizeof(int64_t), alignof(int64_t));
  h.partial_results_off = layout.append(partial_result_count, sizeof(double), alignof(double));

  if (!layout.valid)
    return false;
  *out = h;
  *out_bytes = layout.cursor;
  return true;
}

size_t hashagg_forced_test_chunk_rows() {
  const char* raw = std::getenv(HASH_AGG_STREAM_TEST_CHUNK_ENV);
  if (raw == nullptr || *raw == '\0')
    return 0;
  char* end = nullptr;
  const unsigned long long parsed = std::strtoull(raw, &end, 10);
  if (end == raw || *end != '\0' || parsed == 0)
    return 0;
  if (parsed > static_cast<unsigned long long>(std::numeric_limits<size_t>::max()))
    return std::numeric_limits<size_t>::max();
  return static_cast<size_t>(parsed);
}

void fill_hashagg_streaming_metadata(uint8_t* slab, const int* value_types,
                                     const uint8_t* const* value_nulls,
                                     const pgaccel_agg_col* agg_cols) {
  auto* h = reinterpret_cast<HashAggStreamingSlabHeader*>(slab);
  auto* value_offsets = reinterpret_cast<size_t*>(slab + h->value_offsets_off);
  auto* null_offsets = reinterpret_cast<size_t*>(slab + h->null_offsets_off);
  auto* null_present = reinterpret_cast<uint8_t*>(slab + h->null_present_off);
  auto* staged_types = reinterpret_cast<int*>(slab + h->value_types_off);
  auto* funcs = reinterpret_cast<pgaccel_agg_func*>(slab + h->agg_funcs_off);
  auto* col_idx = reinterpret_cast<size_t*>(slab + h->agg_col_idx_off);

  size_t value_cursor = h->chunk_value_data_off;
  size_t null_cursor = h->chunk_null_data_off;
  for (size_t a = 0; a < h->num_aggs; ++a) {
    const bool reads_value = agg_reads_value(agg_cols[a].func, agg_cols[a].col_idx);
    const size_t elem_size = value_elem_size(value_types[a]);
    size_t aligned = value_cursor;
    if (reads_value) {
      checked_align_up(value_cursor, elem_size, &aligned);
      value_cursor = aligned + h->chunk_capacity * elem_size;
    }
    value_offsets[a] = reads_value ? aligned - h->chunk_value_data_off : 0;
    const bool has_nulls = reads_value && value_nulls != nullptr && value_nulls[a] != nullptr;
    null_offsets[a] = has_nulls ? null_cursor - h->chunk_null_data_off : 0;
    if (has_nulls)
      null_cursor += h->chunk_capacity;
    null_present[a] = has_nulls ? 1 : 0;
    staged_types[a] = value_types[a];
    funcs[a] = agg_cols[a].func;
    col_idx[a] = agg_cols[a].col_idx;
  }
}

struct HashAggStreamingPlan {
  HashAggStreamingSlabHeader layout{};
  size_t chunk_capacity = 0;
  size_t slab_bytes = 0;
};

template <typename KeyT>
bool build_hashagg_streaming_plan(size_t row_count, size_t num_aggs,
                                  const uint8_t* const* value_nulls,
                                  const int* value_types, const pgaccel_agg_col* agg_cols,
                                  size_t slab_budget, HashAggStreamingPlan* out) {
  if (out == nullptr)
    return false;

  const size_t distinct_ceiling = std::min(row_count, HASH_AGG_STREAM_MAX_TABLE_SLOTS / 2);
  size_t table_need = 0;
  size_t table_capacity = 0;
  if (!checked_mul_size(distinct_ceiling, 2, &table_need) ||
      !next_power_of_two_size(std::max<size_t>(table_need, 2), &table_capacity)) {
    return false;
  }

  size_t per_row_bytes = sizeof(KeyT) + sizeof(uint8_t);
  for (size_t a = 0; a < num_aggs; ++a) {
    if (!agg_reads_value(agg_cols[a].func, agg_cols[a].col_idx))
      continue;
    if (!checked_add_size(per_row_bytes, value_elem_size(value_types[a]), &per_row_bytes) ||
        (value_nulls != nullptr && value_nulls[a] != nullptr &&
         !checked_add_size(per_row_bytes, sizeof(uint8_t), &per_row_bytes))) {
      return false;
    }
  }

  HashAggStreamingSlabHeader layout{};
  size_t base_bytes = 0;
  while (!build_hashagg_streaming_layout(table_capacity, num_aggs, 0, sizeof(KeyT), value_nulls,
                                         value_types, agg_cols, &layout, &base_bytes) ||
         base_bytes >= slab_budget) {
    if (table_capacity <= 2)
      return false;
    table_capacity /= 2;
  }

  const size_t upper = std::min(row_count, (slab_budget - base_bytes) / per_row_bytes);
  if (upper == 0)
    return false;

  size_t low = 1;
  size_t high = upper;
  size_t chunk_capacity = 0;
  size_t slab_bytes = 0;
  while (low <= high) {
    const size_t mid = low + (high - low) / 2;
    HashAggStreamingSlabHeader candidate{};
    size_t candidate_bytes = 0;
    if (build_hashagg_streaming_layout(table_capacity, num_aggs, mid, sizeof(KeyT), value_nulls,
                                       value_types, agg_cols, &candidate, &candidate_bytes) &&
        candidate_bytes <= slab_budget) {
      chunk_capacity = mid;
      slab_bytes = candidate_bytes;
      layout = candidate;
      low = mid + 1;
    } else {
      high = mid - 1;
    }
  }
  if (chunk_capacity == 0)
    return false;

  const size_t forced_rows = hashagg_forced_test_chunk_rows();
  if (forced_rows != 0 && forced_rows < chunk_capacity) {
    chunk_capacity = forced_rows;
    if (!build_hashagg_streaming_layout(table_capacity, num_aggs, chunk_capacity, sizeof(KeyT),
                                        value_nulls, value_types, agg_cols, &layout, &slab_bytes)) {
      return false;
    }
  }

  out->layout = layout;
  out->chunk_capacity = chunk_capacity;
  out->slab_bytes = slab_bytes;
  return true;
}

template <typename KeyT>
pgaccel_status agg_hash_streaming_numeric(const void* group_keys, const uint8_t* group_null_mask,
                                          size_t row_count, int key_type,
                                          const void* const* value_cols,
                                          const uint8_t* const* value_nulls, const int* value_types,
                                          const pgaccel_agg_col* agg_cols, size_t num_aggs,
                                          pgaccel_agg_state** out_state) {
  if (out_state == nullptr)
    return PGACCEL_ERROR;
  if (row_count == 0 || num_aggs == 0) {
    *out_state = nullptr;
    return PGACCEL_ERROR;
  }
  sycl::queue& q = pgaccel_require_queue();
  const pgaccel_platform_caps caps = pgaccel_get_caps();
  if (caps.max_alloc_bytes < sizeof(HashAggStreamingSlabHeader) * 2) {
    *out_state = nullptr;
    return PGACCEL_OOM;
  }

  const size_t slab_budget = std::min(HASH_AGG_STREAM_MAX_SLAB_BYTES, caps.max_alloc_bytes / 2);
  HashAggStreamingPlan plan;
  if (!build_hashagg_streaming_plan<KeyT>(row_count, num_aggs, value_nulls, value_types, agg_cols,
                                          slab_budget, &plan)) {
    *out_state = nullptr;
    return PGACCEL_OOM;
  }
  const HashAggStreamingSlabHeader& layout = plan.layout;
  const size_t chunk_capacity = plan.chunk_capacity;
  const size_t slab_bytes = plan.slab_bytes;

  uint8_t* slab = sycl::malloc_shared<uint8_t>(slab_bytes, q);
  if (slab == nullptr) {
    *out_state = nullptr;
    return PGACCEL_OOM;
  }
  *reinterpret_cast<HashAggStreamingSlabHeader*>(slab) = layout;
  fill_hashagg_streaming_metadata(slab, value_types, value_nulls, agg_cols);

  try {
    q.parallel_for(sycl::range<1>(layout.table_capacity), [=](sycl::id<1> id) {
       auto* h = reinterpret_cast<HashAggStreamingSlabHeader*>(slab);
       auto* slot_used = reinterpret_cast<uint8_t*>(slab + h->slot_used_off);
       slot_used[id[0]] = 0;
       if (id[0] == 0) {
         h->group_count = 0;
         h->null_group = HASH_AGG_STREAM_NO_GROUP;
         h->overflow = 0;
       }
     }).wait_and_throw();
    pgaccel_record_gpu_exec();

    const auto* source_keys = static_cast<const uint8_t*>(group_keys);
    const auto* value_offsets = reinterpret_cast<const size_t*>(slab + layout.value_offsets_off);
    const auto* null_offsets = reinterpret_cast<const size_t*>(slab + layout.null_offsets_off);
    const auto* null_present = reinterpret_cast<const uint8_t*>(slab + layout.null_present_off);

    size_t row_begin = 0;
    do {
      size_t rows = row_count - row_begin;
      if (rows > chunk_capacity)
        rows = chunk_capacity;
      q.memcpy(slab + layout.chunk_keys_off, source_keys + row_begin * sizeof(KeyT),
               rows * sizeof(KeyT))
          .wait_and_throw();
      if (group_null_mask != nullptr) {
        q.memcpy(slab + layout.chunk_key_nulls_off, group_null_mask + row_begin, rows)
            .wait_and_throw();
      } else {
        q.memset(slab + layout.chunk_key_nulls_off, 0, rows).wait_and_throw();
      }

      size_t staged_agg = 0;
      do {
        const bool reads_value = agg_cols[staged_agg].func != PGACCEL_AGG_COUNT ||
                                 agg_cols[staged_agg].col_idx != SIZE_MAX;
        if (reads_value) {
          const size_t elem_size = sizeof(double);
          const auto* source = static_cast<const uint8_t*>(value_cols[staged_agg]);
          q.memcpy(slab + layout.chunk_value_data_off + value_offsets[staged_agg],
                   source + row_begin * elem_size, rows * elem_size)
              .wait_and_throw();
          if (null_present[staged_agg] != 0) {
            q.memcpy(slab + layout.chunk_null_data_off + null_offsets[staged_agg],
                     value_nulls[staged_agg] + row_begin, rows)
                .wait_and_throw();
          }
        }
        ++staged_agg;
      } while (staged_agg < num_aggs);
      const size_t chunk_blocks = 1 + (rows - 1) / HASH_AGG_STREAM_BLOCK_ROWS;
      q.memcpy(&reinterpret_cast<HashAggStreamingSlabHeader*>(slab)->chunk_rows, &rows,
               sizeof(rows))
          .wait_and_throw();
      q.memcpy(&reinterpret_cast<HashAggStreamingSlabHeader*>(slab)->chunk_blocks, &chunk_blocks,
               sizeof(chunk_blocks))
          .wait_and_throw();

      // Each work-item owns one fixed-size row block and its disjoint scratch
      // table. No atomics are needed, including for f64 aggregate state.
      q.parallel_for(sycl::range<1>(chunk_blocks), [=](sycl::id<1> id) {
         auto* h = reinterpret_cast<HashAggStreamingSlabHeader*>(slab);
         if (h->overflow != 0)
           return;
         const size_t block = id[0];
         const size_t row_start = block * HASH_AGG_STREAM_BLOCK_ROWS;
         const size_t row_end = sycl::min(row_start + HASH_AGG_STREAM_BLOCK_ROWS, h->chunk_rows);
         const size_t table_base = block * HASH_AGG_STREAM_BLOCK_TABLE_SLOTS;
         const size_t partial_base = row_start;

         auto* local_slot_used =
             reinterpret_cast<uint8_t*>(slab + h->block_slot_used_off) + table_base;
         auto* local_slot_bits =
             reinterpret_cast<uint64_t*>(slab + h->block_slot_bits_off) + table_base;
         auto* local_slot_groups =
             reinterpret_cast<uint32_t*>(slab + h->block_slot_groups_off) + table_base;
         auto* block_group_counts = reinterpret_cast<uint32_t*>(slab + h->block_group_counts_off);
         auto* partial_keys = reinterpret_cast<KeyT*>(slab + h->partial_keys_off);
         auto* partial_key_nulls = reinterpret_cast<uint8_t*>(slab + h->partial_key_nulls_off);
         auto* partial_counts = reinterpret_cast<int64_t*>(slab + h->partial_counts_off);
         auto* partial_results = reinterpret_cast<double*>(slab + h->partial_results_off);
         const auto* staged_value_offsets =
             reinterpret_cast<const size_t*>(slab + h->value_offsets_off);
         const auto* staged_null_offsets =
             reinterpret_cast<const size_t*>(slab + h->null_offsets_off);
         const auto* staged_null_present =
             reinterpret_cast<const uint8_t*>(slab + h->null_present_off);
         const auto* staged_value_types = reinterpret_cast<const int*>(slab + h->value_types_off);
         const auto* staged_funcs =
             reinterpret_cast<const pgaccel_agg_func*>(slab + h->agg_funcs_off);
         const auto* staged_col_idx = reinterpret_cast<const size_t*>(slab + h->agg_col_idx_off);
         const auto* chunk_keys = reinterpret_cast<const KeyT*>(slab + h->chunk_keys_off);
         const auto* chunk_key_nulls =
             reinterpret_cast<const uint8_t*>(slab + h->chunk_key_nulls_off);
         const auto* chunk_values = slab + h->chunk_value_data_off;
         const auto* chunk_nulls = slab + h->chunk_null_data_off;
         for (size_t slot = 0; slot < HASH_AGG_STREAM_BLOCK_TABLE_SLOTS; ++slot)
           local_slot_used[slot] = 0;

         uint32_t local_group_count = 0;
         uint32_t local_null_group = HASH_AGG_STREAM_NO_GROUP;
         for (size_t row = row_start; row < row_end; ++row) {
           uint32_t local_group = HASH_AGG_STREAM_NO_GROUP;
           bool new_group = false;
           if (chunk_key_nulls[row] != 0) {
             local_group = local_null_group;
             if (local_group == HASH_AGG_STREAM_NO_GROUP) {
               local_group = local_group_count++;
               local_null_group = local_group;
               new_group = true;
             }
           } else {
             const KeyT key = chunk_keys[row];
             const uint64_t bits = group_key_bits(key);
             size_t slot =
                 static_cast<size_t>(hash64(bits)) & (HASH_AGG_STREAM_BLOCK_TABLE_SLOTS - 1);
             for (size_t probe = 0; probe < HASH_AGG_STREAM_BLOCK_TABLE_SLOTS; ++probe) {
               if (local_slot_used[slot] == 0) {
                 local_group = local_group_count++;
                 local_slot_used[slot] = 1;
                 local_slot_bits[slot] = bits;
                 local_slot_groups[slot] = local_group;
                 new_group = true;
                 break;
               }
               if (local_slot_bits[slot] == bits) {
                 local_group = local_slot_groups[slot];
                 break;
               }
               slot = (slot + 1) & (HASH_AGG_STREAM_BLOCK_TABLE_SLOTS - 1);
             }
           }

           const size_t partial = partial_base + local_group;
           if (new_group) {
             partial_keys[partial] = chunk_key_nulls[row] != 0 ? KeyT{} : chunk_keys[row];
             partial_key_nulls[partial] = chunk_key_nulls[row] != 0 ? 1 : 0;
             partial_counts[partial] = 0;
             for (size_t a = 0; a < h->num_aggs; ++a) {
               double neutral = 0.0;
               if (staged_funcs[a] == PGACCEL_AGG_MIN)
                 neutral = std::numeric_limits<double>::infinity();
               else if (staged_funcs[a] == PGACCEL_AGG_MAX)
                 neutral = -std::numeric_limits<double>::infinity();
               partial_results[a * h->chunk_capacity + partial] = neutral;
             }
           }
           ++partial_counts[partial];

           for (size_t a = 0; a < h->num_aggs; ++a) {
             const pgaccel_agg_func func = staged_funcs[a];
             double* result = partial_results + a * h->chunk_capacity + partial;
             if (func == PGACCEL_AGG_COUNT && staged_col_idx[a] == SIZE_MAX) {
               *result += 1.0;
               continue;
             }
             const val_read value = device_read_value_flat(
                 chunk_values, chunk_nulls, staged_value_offsets[a], staged_null_offsets[a],
                 staged_null_present[a] != 0, row, staged_value_types[a]);
             if (value.is_null)
               continue;
             switch (func) {
               case PGACCEL_AGG_SUM:
                 *result += value.value;
                 break;
               case PGACCEL_AGG_MIN:
                 if (value.value < *result)
                   *result = value.value;
                 break;
               case PGACCEL_AGG_MAX:
                 if (value.value > *result)
                   *result = value.value;
                 break;
               case PGACCEL_AGG_COUNT:
                 *result += 1.0;
                 break;
               case PGACCEL_AGG_AVG:
               case PGACCEL_AGG_STDDEV:
               case PGACCEL_AGG_VAR:
                 break;
             }
           }
         }
         block_group_counts[block] = local_group_count;
       }).wait_and_throw();
      pgaccel_record_gpu_exec();

      // Merge only block-level groups, in block/first-seen order, into the
      // persistent table. This is the sole cross-block and cross-chunk merge.
      q.single_task([=]() {
         auto* h = reinterpret_cast<HashAggStreamingSlabHeader*>(slab);
         if (h->overflow != 0)
           return;
         auto* slot_used = reinterpret_cast<uint8_t*>(slab + h->slot_used_off);
         auto* slot_bits = reinterpret_cast<uint64_t*>(slab + h->slot_bits_off);
         auto* slot_groups = reinterpret_cast<uint32_t*>(slab + h->slot_groups_off);
         auto* persistent_keys = reinterpret_cast<KeyT*>(slab + h->group_keys_off);
         auto* group_counts = reinterpret_cast<int64_t*>(slab + h->group_counts_off);
         auto* results = reinterpret_cast<double*>(slab + h->results_off);
         const auto* funcs = reinterpret_cast<const pgaccel_agg_func*>(slab + h->agg_funcs_off);
         const auto* block_group_counts =
             reinterpret_cast<const uint32_t*>(slab + h->block_group_counts_off);
         const auto* partial_keys = reinterpret_cast<const KeyT*>(slab + h->partial_keys_off);
         const auto* partial_key_nulls =
             reinterpret_cast<const uint8_t*>(slab + h->partial_key_nulls_off);
         const auto* partial_counts =
             reinterpret_cast<const int64_t*>(slab + h->partial_counts_off);
         const auto* partial_results =
             reinterpret_cast<const double*>(slab + h->partial_results_off);
         const size_t mask = h->table_capacity - 1;
         uint32_t next_group = h->group_count;
         uint32_t null_group = h->null_group;

         for (size_t block = 0; block < h->chunk_blocks; ++block) {
           const size_t partial_base = block * HASH_AGG_STREAM_BLOCK_ROWS;
           for (uint32_t local_group = 0; local_group < block_group_counts[block]; ++local_group) {
             const size_t partial = partial_base + local_group;
             uint32_t group = HASH_AGG_STREAM_NO_GROUP;
             bool new_group = false;
             if (partial_key_nulls[partial] != 0) {
               group = null_group;
               if (group == HASH_AGG_STREAM_NO_GROUP) {
                 if (next_group >= h->max_groups) {
                   h->group_count = next_group;
                   h->null_group = null_group;
                   h->overflow = HASH_AGG_STREAM_OVERFLOW_GROUPS;
                   return;
                 }
                 group = next_group++;
                 null_group = group;
                 persistent_keys[group] = KeyT{};
                 new_group = true;
               }
             } else {
               const KeyT key = partial_keys[partial];
               const uint64_t bits = group_key_bits(key);
               size_t slot = static_cast<size_t>(hash64(bits)) & mask;
               for (size_t probe = 0; probe < h->table_capacity; ++probe) {
                 if (slot_used[slot] == 0) {
                   if (next_group >= h->max_groups) {
                     h->group_count = next_group;
                     h->null_group = null_group;
                     h->overflow = HASH_AGG_STREAM_OVERFLOW_GROUPS;
                     return;
                   }
                   group = next_group++;
                   slot_used[slot] = 1;
                   slot_bits[slot] = bits;
                   slot_groups[slot] = group;
                   persistent_keys[group] = key;
                   new_group = true;
                   break;
                 }
                 if (slot_bits[slot] == bits) {
                   group = slot_groups[slot];
                   break;
                 }
                 slot = (slot + 1) & mask;
               }
               if (group == HASH_AGG_STREAM_NO_GROUP) {
                 h->group_count = next_group;
                 h->null_group = null_group;
                 h->overflow = HASH_AGG_STREAM_OVERFLOW_TABLE;
                 return;
               }
             }

             if (new_group) {
               group_counts[group] = 0;
               for (size_t a = 0; a < h->num_aggs; ++a) {
                 double neutral = 0.0;
                 if (funcs[a] == PGACCEL_AGG_MIN)
                   neutral = std::numeric_limits<double>::infinity();
                 else if (funcs[a] == PGACCEL_AGG_MAX)
                   neutral = -std::numeric_limits<double>::infinity();
                 results[a * h->max_groups + group] = neutral;
               }
             }
             if (partial_counts[partial] >
                 std::numeric_limits<int64_t>::max() - group_counts[group]) {
               h->group_count = next_group;
               h->null_group = null_group;
               h->overflow = HASH_AGG_STREAM_OVERFLOW_COUNT;
               return;
             }
             group_counts[group] += partial_counts[partial];
             for (size_t a = 0; a < h->num_aggs; ++a) {
               const double partial_value = partial_results[a * h->chunk_capacity + partial];
               double* result = results + a * h->max_groups + group;
               if (funcs[a] == PGACCEL_AGG_MIN) {
                 if (partial_value < *result)
                   *result = partial_value;
               } else if (funcs[a] == PGACCEL_AGG_MAX) {
                 if (partial_value > *result)
                   *result = partial_value;
               } else {
                 *result += partial_value;
               }
             }
           }
         }
         h->group_count = next_group;
         h->null_group = null_group;
       }).wait_and_throw();
      pgaccel_record_gpu_exec();
      if (reinterpret_cast<HashAggStreamingSlabHeader*>(slab)->overflow != 0)
        break;
      row_begin += chunk_capacity;
    } while (row_begin < row_count);
  } catch (...) {
    sycl::free(slab, q);
    throw;
  }

  auto* h = reinterpret_cast<HashAggStreamingSlabHeader*>(slab);
  if (h->overflow != 0) {
    std::fprintf(stderr,
                 "pgaccel: bounded hash_agg capacity exhausted "
                 "(code=%u groups=%u max_groups=%zu table_slots=%zu)\n",
                 h->overflow, h->group_count, h->max_groups, h->table_capacity);
    sycl::free(slab, q);
    *out_state = nullptr;
    return PGACCEL_OOM;
  }
  if (h->group_count == 0) {
    sycl::free(slab, q);
    *out_state = nullptr;
    return PGACCEL_ERROR;
  }

  auto* state = new (std::nothrow) pgaccel_agg_state();
  if (state == nullptr) {
    sycl::free(slab, q);
    *out_state = nullptr;
    return PGACCEL_OOM;
  }
  try {
    const size_t group_count = h->group_count;
    state->key_size = sizeof(KeyT);
    state->key_type = key_type;
    state->group_count = group_count;
    state->num_aggs = num_aggs;
    const size_t group_key_bytes = group_count * sizeof(KeyT);
    state->group_key_buf.resize(group_key_bytes);
    state->counts.resize(group_count);
    state->results.resize(num_aggs);
    size_t result_index = 0;
    do {
      state->results[result_index].resize(group_count);
      ++result_index;
    } while (result_index < num_aggs);

    const auto* persistent_keys = reinterpret_cast<const uint8_t*>(slab + h->group_keys_off);
    const auto* counts = reinterpret_cast<const int64_t*>(slab + h->group_counts_off);
    const auto* results = reinterpret_cast<const double*>(slab + h->results_off);
    q.memcpy(state->group_key_buf.data(), persistent_keys, group_key_bytes).wait_and_throw();
    q.memcpy(state->counts.data(), counts, group_count * sizeof(int64_t)).wait_and_throw();
    result_index = 0;
    do {
      q.memcpy(state->results[result_index].data(), results + result_index * h->max_groups,
               group_count * sizeof(double))
          .wait_and_throw();
      ++result_index;
    } while (result_index < num_aggs);
  } catch (const std::bad_alloc&) {
    delete state;
    sycl::free(slab, q);
    *out_state = nullptr;
    return PGACCEL_OOM;
  } catch (...) {
    delete state;
    sycl::free(slab, q);
    throw;
  }

  sycl::free(slab, q);
  *out_state = state;
  return PGACCEL_OK;
}

}  // namespace

// ===========================================================================
// Public C API
// ===========================================================================

extern "C" {

pgaccel_status pgaccel_hash_agg_execute_checked(
    const void* group_keys, const uint8_t* group_null_mask, size_t row_count, int key_type,
    const void* const* value_cols, const uint8_t* const* value_nulls, const int* value_types,
    const pgaccel_agg_col* agg_cols, size_t num_aggs, pgaccel_agg_state** out_state) try {
  if (out_state == nullptr)
    return PGACCEL_ERROR;

  if (!validate_hashagg_inputs(group_keys, row_count, key_type, value_cols, value_types, agg_cols,
                               num_aggs, false)) {
    *out_state = nullptr;
    return PGACCEL_ERROR;
  }
  if (!row_parallel_hashagg_key_supported(key_type, row_count) ||
      !row_parallel_hashagg_agg_shape_supported(value_types, agg_cols, num_aggs)) {
    *out_state = nullptr;
    return PGACCEL_UNSUPPORTED;
  }

  if (key_type == 0)
    return agg_hash_streaming_numeric<int32_t>(group_keys, group_null_mask, row_count, key_type,
                                               value_cols, value_nulls, value_types, agg_cols,
                                               num_aggs, out_state);
  if (key_type == 1)
    return agg_hash_streaming_numeric<int64_t>(group_keys, group_null_mask, row_count, key_type,
                                               value_cols, value_nulls, value_types, agg_cols,
                                               num_aggs, out_state);
  if (key_type == 2)
    return agg_hash_streaming_numeric<double>(group_keys, group_null_mask, row_count, key_type,
                                              value_cols, value_nulls, value_types, agg_cols,
                                              num_aggs, out_state);

  *out_state = nullptr;
  return PGACCEL_UNSUPPORTED;
} catch (const pgaccel_no_device_error&) {
  *out_state = nullptr;
  return PGACCEL_ERROR_NO_DEVICE;
} catch (const std::exception& e) {
  *out_state = nullptr;
  return pgaccel_kernel_failure("pgaccel_hash_agg_execute_checked", &e);
} catch (...) {
  *out_state = nullptr;
  return pgaccel_kernel_failure("pgaccel_hash_agg_execute_checked", nullptr);
}

pgaccel_agg_state* pgaccel_hash_agg_execute(const void* group_keys, const uint8_t* group_null_mask,
                                            size_t row_count, int key_type,
                                            const void* const* value_cols,
                                            const uint8_t* const* value_nulls,
                                            const int* value_types, const pgaccel_agg_col* agg_cols,
                                            size_t num_aggs) {
  pgaccel_agg_state* state = nullptr;
  const pgaccel_status status =
      pgaccel_hash_agg_execute_checked(group_keys, group_null_mask, row_count, key_type, value_cols,
                                       value_nulls, value_types, agg_cols, num_aggs, &state);
  if (status != PGACCEL_OK)
    return nullptr;
  return state;
}

pgaccel_agg_state* pgaccel_hash_count_i64_execute(const int64_t* group_keys,
                                                  const uint8_t* group_null_mask,
                                                  size_t row_count) {
  (void)group_keys;
  (void)group_null_mask;
  (void)row_count;
  // Compatibility ABI only: host-key staging is outside the resident engine
  // contract. Device-buffer callers use the bounded/sorted resident entries.
  return nullptr;
}

pgaccel_agg_state* pgaccel_hash_count_i64_device_execute(int64_t* group_keys, size_t row_count) {
  try {
    return agg_count_i64_sort_reduce_device(group_keys, row_count);
  } catch (const std::exception& e) {
    std::fprintf(stderr, "pgaccel: hash_count_i64_device_execute failed: %s\n", e.what());
  } catch (...) {
    std::fprintf(stderr, "pgaccel: hash_count_i64_device_execute failed (unknown)\n");
  }
  return nullptr;
}

pgaccel_agg_state* pgaccel_hash_count_i64_sorted_device_execute(int64_t* sorted_group_keys,
                                                                size_t row_count) {
  try {
    return agg_count_i64_sorted_device(sorted_group_keys, row_count);
  } catch (const std::exception& e) {
    std::fprintf(stderr, "pgaccel: hash_count_i64_sorted_device_execute failed: %s\n", e.what());
  } catch (...) {
    std::fprintf(stderr, "pgaccel: hash_count_i64_sorted_device_execute failed (unknown)\n");
  }
  return nullptr;
}

pgaccel_agg_state* pgaccel_hash_count_i64_device_hash_execute(int64_t* group_keys,
                                                              size_t row_count) {
  try {
    return agg_count_i64_hash_device(group_keys, row_count, row_count);
  } catch (const std::exception& e) {
    std::fprintf(stderr, "pgaccel: hash_count_i64_device_hash_execute failed: %s\n", e.what());
  } catch (...) {
    std::fprintf(stderr, "pgaccel: hash_count_i64_device_hash_execute failed (unknown)\n");
  }
  return nullptr;
}

pgaccel_agg_state* pgaccel_hash_count_i64_device_hash_execute_bounded(int64_t* group_keys,
                                                                      size_t row_count,
                                                                      size_t max_distinct_hint) {
  try {
    return agg_count_i64_hash_device(group_keys, row_count, max_distinct_hint);
  } catch (const std::exception& e) {
    std::fprintf(stderr, "pgaccel: hash_count_i64_device_hash_execute_bounded failed: %s\n",
                 e.what());
  } catch (...) {
    std::fprintf(stderr, "pgaccel: hash_count_i64_device_hash_execute_bounded failed (unknown)\n");
  }
  return nullptr;
}

pgaccel_status pgaccel_hash_agg_execute_sort_based(
    const void* group_keys, const uint8_t* group_null_mask, size_t row_count, int key_type,
    const void* const* value_cols, const uint8_t* const* value_nulls, const int* value_types,
    const pgaccel_agg_col* agg_cols, size_t num_aggs, pgaccel_agg_state** out_state) try {
  if (out_state == nullptr)
    return PGACCEL_ERROR;
  *out_state = nullptr;

  if (!validate_hashagg_inputs(group_keys, row_count, key_type, value_cols, value_types, agg_cols,
                               num_aggs, false))
    return PGACCEL_ERROR;
  (void)group_null_mask;
  (void)value_nulls;
  return PGACCEL_UNSUPPORTED;
} catch (const pgaccel_no_device_error&) {
  return PGACCEL_ERROR_NO_DEVICE;
} catch (const std::exception& e) {
  return pgaccel_kernel_failure("pgaccel_hash_agg_execute_sort_based", &e);
} catch (...) {
  return pgaccel_kernel_failure("pgaccel_hash_agg_execute_sort_based", nullptr);
}

size_t pgaccel_agg_group_count(const pgaccel_agg_state* state) {
  if (state == nullptr)
    return 0;
  return state->group_count;
}

const void* pgaccel_agg_get_group_keys(const pgaccel_agg_state* state) {
  if (state == nullptr)
    return nullptr;
  return state->group_key_buf.data();
}

const double* pgaccel_agg_get_results(const pgaccel_agg_state* state, size_t agg_idx) {
  if (state == nullptr || agg_idx >= state->num_aggs)
    return nullptr;
  if (state->results.size() <= agg_idx)
    return nullptr;
  return state->results[agg_idx].data();
}

pgaccel_status pgaccel_hash_agg_execute_partial_checked(
    const void* group_keys, const uint8_t* group_null_mask, size_t row_count, int key_type,
    const void* const* value_cols, const uint8_t* const* value_nulls, const int* value_types,
    const pgaccel_agg_col* agg_cols, size_t num_aggs, pgaccel_agg_state** out_state) try {
  if (out_state == nullptr)
    return PGACCEL_ERROR;
  *out_state = nullptr;

  if (!validate_hashagg_inputs(group_keys, row_count, key_type, value_cols, value_types, agg_cols,
                               num_aggs, true))
    return PGACCEL_ERROR;
  (void)group_null_mask;
  (void)value_nulls;
  return PGACCEL_UNSUPPORTED;
} catch (const pgaccel_no_device_error&) {
  return PGACCEL_ERROR_NO_DEVICE;
} catch (const std::exception& e) {
  return pgaccel_kernel_failure("pgaccel_hash_agg_execute_partial_checked", &e);
} catch (...) {
  return pgaccel_kernel_failure("pgaccel_hash_agg_execute_partial_checked", nullptr);
}

pgaccel_agg_state*
pgaccel_hash_agg_execute_partial(const void* group_keys, const uint8_t* group_null_mask,
                                 size_t row_count, int key_type, const void* const* value_cols,
                                 const uint8_t* const* value_nulls, const int* value_types,
                                 const pgaccel_agg_col* agg_cols, size_t num_aggs) {
  (void)group_keys;
  (void)group_null_mask;
  (void)row_count;
  (void)key_type;
  (void)value_cols;
  (void)value_nulls;
  (void)value_types;
  (void)agg_cols;
  (void)num_aggs;
  return nullptr;
}

const double* pgaccel_agg_get_partial_results(const pgaccel_agg_state* state, size_t agg_idx) {
  if (state == nullptr || agg_idx >= state->num_aggs)
    return nullptr;
  if (state->partial_results.size() <= agg_idx)
    return nullptr;
  return state->partial_results[agg_idx].data();
}

size_t pgaccel_agg_get_partial_width(const pgaccel_agg_state* state, size_t agg_idx) {
  if (state == nullptr || agg_idx >= state->num_aggs)
    return 0;
  if (state->partial_widths.size() > agg_idx)
    return state->partial_widths[agg_idx];
  return 0;
}

const int64_t* pgaccel_agg_get_counts(const pgaccel_agg_state* state) {
  if (state == nullptr)
    return nullptr;
  return state->counts.data();
}

void pgaccel_agg_free(pgaccel_agg_state* state) try {
  delete state;
} catch (const std::exception& e) {
  std::fprintf(stderr, "pgaccel: pgaccel_agg_free failed: %s\n", e.what());
} catch (...) {
  std::fprintf(stderr, "pgaccel: pgaccel_agg_free failed: unknown C++ exception\n");
}

}  // extern "C"

/*
 * hash_agg.cpp — GPU hash aggregation: grouped SUM/MIN/MAX/COUNT.
 *
 * Two paths:
 *   - `agg_hash`: hash-table group assignment + SYCL accumulation kernel
 *     (one work-item per row, atomic accumulation per group).
 *   - `agg_sort_based`: GPU sort by group key → boundary scan → SYCL
 *     accumulation kernel (one work-item per group, sequential per-group
 *     scan over the contiguous sorted range).
 *
 * All accumulators use f64 internally to prevent integer overflow
 * (int32 SUM can overflow int32 after ~2B rows; f64 gives ~15 digits
 * of precision which is sufficient for partial aggregates).
 *
 * NULL group keys are accumulated into a single "NULL group" (like PG).
 * NULL values are skipped for SUM/MIN/MAX but not for COUNT(*).
 *
 * Per CLAUDE.md rules #11/#12 (GPU-only, SYCL-only) — both paths are
 * SYCL kernels. The host-side hash table and boundary scan that remain
 * are orchestration glue that decides which rows belong to which
 * group, not row-iteration "kernels". Once the row-to-group mapping is
 * known, all per-row work runs as a `sycl::parallel_for`.
 */

#include <sycl/sycl.hpp>

#include <algorithm>
#include <cmath>
#include <cstdint>
#include <cstdio>
#include <cstring>
#include <exception>
#include <limits>
#include <new>
#include <unordered_map>
#include <vector>

#include "pgaccel_ffi.h"
#include "pgaccel_hash_agg.h"

extern sycl::queue* g_queue;

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

// ---------------------------------------------------------------------------
// Group-key hash + equality (host side)
// ---------------------------------------------------------------------------

static inline uint64_t read_key_u64(const void* keys, size_t row, int key_type) {
  switch (key_type) {
    case 0: {  // INT32
      int32_t k = static_cast<const int32_t*>(keys)[row];
      return hash64(static_cast<uint64_t>(static_cast<uint32_t>(k)));
    }
    case 1: {  // INT64
      int64_t k = static_cast<const int64_t*>(keys)[row];
      return hash64(static_cast<uint64_t>(k));
    }
    case 2: {  // FLOAT64
      double k = static_cast<const double*>(keys)[row];
      if (k != k)
        return hash64(0x7FF8000000000000ULL);
      if (k == 0.0)
        k = 0.0;
      uint64_t bits;
      memcpy(&bits, &k, sizeof(bits));
      return hash64(bits);
    }
    case 4: {  // UUID (16 bytes)
      // Read both u64 halves, mix each through hash64, XOR. Byte-equal
      // UUIDs map to identical hashes; any single bit flip propagates
      // through hash64's full diffusion.
      const uint8_t* p = static_cast<const uint8_t*>(keys) + row * 16;
      uint64_t lo, hi;
      memcpy(&lo, p, 8);
      memcpy(&hi, p + 8, 8);
      return hash64(lo) ^ hash64(hi);
    }
    case 5: {  // INET / CIDR (24-byte canonical key)
      // Layout: family(1) + bits(1) + ipaddr(16, IPv4 zero-padded) +
      //         pad(6) = 24 bytes = 3 uint64_t. Mix each via hash64
      //         and XOR for full diffusion.
      const uint8_t* p = static_cast<const uint8_t*>(keys) + row * 24;
      uint64_t a, b, c;
      memcpy(&a, p, 8);
      memcpy(&b, p + 8, 8);
      memcpy(&c, p + 16, 8);
      return hash64(a) ^ hash64(b) ^ hash64(c);
    }
    default:
      return 0;
  }
}

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

static inline bool key_bytes_equal(const uint8_t* stored, const uint8_t* current, size_t ksz,
                                   int key_type) {
  if (key_type != 2)
    return memcmp(stored, current, ksz) == 0;

  double a;
  double b;
  memcpy(&a, stored, sizeof(a));
  memcpy(&b, current, sizeof(b));
  const bool a_nan = (a != a);
  const bool b_nan = (b != b);
  return (a_nan && b_nan) || a == b;
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
// Why a flat buffer (vs an array of typed pointers): on Apple Metal the
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

/// If a large batch cannot use the sort path, the alternate GPU accumulation
/// kernel is admitted only for low-cardinality cases. High-cardinality large
/// batches return unsupported rather than running a CPU-backed pg_accel plan.
static constexpr size_t HASH_AGG_MAX_LARGE_UNSORTED_GROUPS = 4096;

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
  double value;
  std::memcpy(&value, &bits, sizeof(value));
  return value;
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

  std::vector<uint32_t> slot_to_group(slot_count, HASH_AGG_GROUP_NONE);
  uint32_t n_groups = 0;
  for (size_t slot = 0; slot < slot_count; ++slot) {
    const uint32_t count = d_slot_counts[slot];
    if (count == 0)
      continue;
    if (static_cast<size_t>(n_groups) >= row_count) {
      sycl::free(d_keys, q);
      sycl::free(d_key_nulls, q);
      sycl::free(d_slot_bits, q);
      sycl::free(d_slot_counts, q);
      sycl::free(d_overflow, q);
      return false;
    }
    const uint32_t group_id = n_groups++;
    slot_to_group[slot] = group_id;
    group_counts[group_id] = count;
    if (slot == static_cast<size_t>(null_slot)) {
      group_key_out[group_id] = KeyT{};
    } else if (slot == static_cast<size_t>(special_bits_slot)) {
      group_key_out[group_id] = group_key_from_bits<KeyT>(HASH_AGG_SLOT_BITS_EMPTY);
    } else {
      group_key_out[group_id] = group_key_from_bits<KeyT>(d_slot_bits[slot]);
    }
  }
  *out_group_count = n_groups;

  uint32_t* d_slot_to_group = sycl::malloc_shared<uint32_t>(slot_count, q);
  if (d_slot_to_group == nullptr) {
    sycl::free(d_keys, q);
    sycl::free(d_key_nulls, q);
    sycl::free(d_slot_bits, q);
    sycl::free(d_slot_counts, q);
    sycl::free(d_overflow, q);
    return false;
  }
  std::memcpy(d_slot_to_group, slot_to_group.data(), slot_count * sizeof(uint32_t));

  try {
    q.parallel_for(sycl::range<1>(row_count), [=](sycl::id<1> id) {
       const uint32_t r = static_cast<uint32_t>(id[0]);
       const uint32_t slot = row_to_group[r];
       if (slot != HASH_AGG_GROUP_NONE)
         row_to_group[r] = d_slot_to_group[slot];
     }).wait_and_throw();
  } catch (const std::exception& e) {
    std::fprintf(stderr, "pgaccel: hash_agg row-parallel remap kernel failed: %s\n", e.what());
    sycl::free(d_slot_to_group, q);
    sycl::free(d_keys, q);
    sycl::free(d_key_nulls, q);
    sycl::free(d_slot_bits, q);
    sycl::free(d_slot_counts, q);
    sycl::free(d_overflow, q);
    return false;
  } catch (...) {
    std::fprintf(stderr, "pgaccel: hash_agg row-parallel remap kernel failed (unknown)\n");
    sycl::free(d_slot_to_group, q);
    sycl::free(d_keys, q);
    sycl::free(d_key_nulls, q);
    sycl::free(d_slot_bits, q);
    sycl::free(d_slot_counts, q);
    sycl::free(d_overflow, q);
    return false;
  }

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
  sycl::queue* q = g_queue;
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

  std::vector<size_t> group_starts_h(n_groups);
  std::vector<size_t> group_ends_h(n_groups);
  std::vector<uint32_t> scatter_starts_h(n_groups);
  size_t cursor = 0;
  for (size_t g = 0; g < n_groups; ++g) {
    group_starts_h[g] = cursor;
    scatter_starts_h[g] = static_cast<uint32_t>(cursor);
    cursor += static_cast<size_t>(d_group_counts[g]);
    group_ends_h[g] = cursor;
  }
  if (cursor != row_count) {
    std::fprintf(stderr,
                 "pgaccel: hash_agg row-parallel grouping count mismatch "
                 "(rows=%zu compacted=%zu groups=%zu)\n",
                 row_count, cursor, n_groups);
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
  std::memcpy(d_group_starts, group_starts_h.data(), n_groups * sizeof(size_t));
  std::memcpy(d_group_ends, group_ends_h.data(), n_groups * sizeof(size_t));
  std::memcpy(d_scatter_offsets, scatter_starts_h.data(), n_groups * sizeof(uint32_t));

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

static pgaccel_agg_state* agg_count_i64_hash(const int64_t* group_keys,
                                             const uint8_t* group_null_mask, size_t row_count) {
  sycl::queue* q = g_queue;
  if (q == nullptr || group_keys == nullptr || row_count == 0 ||
      row_count > static_cast<size_t>(std::numeric_limits<uint32_t>::max()))
    return nullptr;

  size_t table_need = 0;
  size_t table_capacity = 0;
  if (!checked_mul_size(row_count, 4, &table_need) ||
      !next_power_of_two_size(table_need, &table_capacity) ||
      table_capacity > static_cast<size_t>(std::numeric_limits<uint32_t>::max())) {
    return nullptr;
  }

  int64_t* d_input_keys = sycl::malloc_shared<int64_t>(row_count, *q);
  uint8_t* d_key_nulls = sycl::malloc_shared<uint8_t>(row_count, *q);
  uint32_t* d_slot_owners = sycl::malloc_shared<uint32_t>(table_capacity, *q);
  uint32_t* d_slot_counts = sycl::malloc_shared<uint32_t>(table_capacity, *q);
  int64_t* d_out_keys = sycl::malloc_shared<int64_t>(row_count, *q);
  uint32_t* d_out_counts = sycl::malloc_shared<uint32_t>(row_count, *q);
  uint32_t* d_group_count = sycl::malloc_shared<uint32_t>(1, *q);
  uint32_t* d_null_count = sycl::malloc_shared<uint32_t>(1, *q);
  uint32_t* d_overflow = sycl::malloc_shared<uint32_t>(1, *q);

  auto cleanup = [&]() {
    if (d_input_keys)
      sycl::free(d_input_keys, *q);
    if (d_key_nulls)
      sycl::free(d_key_nulls, *q);
    if (d_slot_owners)
      sycl::free(d_slot_owners, *q);
    if (d_slot_counts)
      sycl::free(d_slot_counts, *q);
    if (d_out_keys)
      sycl::free(d_out_keys, *q);
    if (d_out_counts)
      sycl::free(d_out_counts, *q);
    if (d_group_count)
      sycl::free(d_group_count, *q);
    if (d_null_count)
      sycl::free(d_null_count, *q);
    if (d_overflow)
      sycl::free(d_overflow, *q);
  };

  if (d_input_keys == nullptr || d_key_nulls == nullptr || d_slot_owners == nullptr ||
      d_slot_counts == nullptr || d_out_keys == nullptr || d_out_counts == nullptr ||
      d_group_count == nullptr || d_null_count == nullptr || d_overflow == nullptr) {
    cleanup();
    return nullptr;
  }

  std::memcpy(d_input_keys, group_keys, row_count * sizeof(int64_t));
  if (group_null_mask != nullptr)
    std::memcpy(d_key_nulls, group_null_mask, row_count);
  else
    std::memset(d_key_nulls, 0, row_count);
  std::fill(d_slot_owners, d_slot_owners + table_capacity, HASH_AGG_GROUP_NONE);
  std::memset(d_slot_counts, 0, table_capacity * sizeof(uint32_t));
  std::memset(d_out_keys, 0, row_count * sizeof(int64_t));
  std::memset(d_out_counts, 0, row_count * sizeof(uint32_t));
  *d_group_count = 0;
  *d_null_count = 0;
  *d_overflow = 0;

  const uint32_t mask = static_cast<uint32_t>(table_capacity - 1);
  const bool has_null_mask = group_null_mask != nullptr;

  try {
    q->parallel_for(sycl::range<1>(row_count), [=](sycl::id<1> id) {
       const uint32_t r = static_cast<uint32_t>(id[0]);
       if (has_null_mask && d_key_nulls[r] != 0) {
         sycl::atomic_ref<uint32_t, sycl::memory_order::relaxed, sycl::memory_scope::device,
                          sycl::access::address_space::global_space>
             null_ref(d_null_count[0]);
         null_ref.fetch_add(1u);
         return;
       }

       const int64_t key = d_input_keys[r];
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

         if (owner != HASH_AGG_GROUP_NONE && d_input_keys[owner] == key) {
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
    std::fprintf(stderr, "pgaccel: hash_count_i64 build kernel failed: %s\n", e.what());
    cleanup();
    return nullptr;
  } catch (...) {
    std::fprintf(stderr, "pgaccel: hash_count_i64 build kernel failed (unknown)\n");
    cleanup();
    return nullptr;
  }

  if (*d_overflow != 0) {
    std::fprintf(stderr,
                 "pgaccel: hash_count_i64 overflow after build "
                 "(rows=%zu nulls=%u first_owner=%u first_count=%u)\n",
                 row_count, *d_null_count, d_slot_owners[0], d_slot_counts[0]);
    cleanup();
    return nullptr;
  }

  try {
    q->parallel_for(sycl::range<1>(table_capacity), [=](sycl::id<1> id) {
       const uint32_t slot = static_cast<uint32_t>(id[0]);
       const uint32_t owner = d_slot_owners[slot];
       if (owner == HASH_AGG_GROUP_NONE)
         return;
       const uint32_t count = d_slot_counts[slot];
       if (count == 0)
         return;

       sycl::atomic_ref<uint32_t, sycl::memory_order::relaxed, sycl::memory_scope::device,
                        sycl::access::address_space::global_space>
           group_ref(d_group_count[0]);
       const uint32_t group_id = group_ref.fetch_add(1u);
       d_out_keys[group_id] = d_input_keys[owner];
       d_out_counts[group_id] = count;
     }).wait_and_throw();
    pgaccel_record_gpu_exec();
  } catch (const std::exception& e) {
    std::fprintf(stderr, "pgaccel: hash_count_i64 compact kernel failed: %s\n", e.what());
    cleanup();
    return nullptr;
  } catch (...) {
    std::fprintf(stderr, "pgaccel: hash_count_i64 compact kernel failed (unknown)\n");
    cleanup();
    return nullptr;
  }

  uint32_t n_groups = *d_group_count;
  if (n_groups == 0) {
    std::fprintf(stderr,
                 "pgaccel: hash_count_i64 produced zero groups "
                 "(rows=%zu overflow=%u nulls=%u first_owner=%u first_count=%u)\n",
                 row_count, *d_overflow, *d_null_count, d_slot_owners[0], d_slot_counts[0]);
  }
  if (*d_null_count != 0) {
    if (static_cast<size_t>(n_groups) >= row_count) {
      cleanup();
      return nullptr;
    }
    d_out_keys[n_groups] = 0;
    d_out_counts[n_groups] = *d_null_count;
    ++n_groups;
  }
  if (n_groups == 0 || static_cast<size_t>(n_groups) > row_count) {
    cleanup();
    return nullptr;
  }

  auto* state = new (std::nothrow) pgaccel_agg_state();
  if (state == nullptr) {
    cleanup();
    return nullptr;
  }

  state->key_size = sizeof(int64_t);
  state->key_type = 1;
  state->group_count = n_groups;
  state->num_aggs = 1;
  state->group_key_buf.resize(static_cast<size_t>(n_groups) * sizeof(int64_t));
  std::memcpy(state->group_key_buf.data(), d_out_keys,
              static_cast<size_t>(n_groups) * sizeof(int64_t));
  state->counts.reserve(n_groups);
  state->results.resize(1);
  state->results[0].reserve(n_groups);
  for (uint32_t i = 0; i < n_groups; ++i) {
    const int64_t count = static_cast<int64_t>(d_out_counts[i]);
    state->counts.push_back(count);
    state->results[0].push_back(static_cast<double>(count));
  }

  cleanup();
  return state;
}

static pgaccel_agg_state* build_count_i64_state_from_u32(const int64_t* out_keys,
                                                         const uint32_t* out_counts,
                                                         uint32_t n_groups) {
  if (out_keys == nullptr || out_counts == nullptr || n_groups == 0)
    return nullptr;

  auto* state = new (std::nothrow) pgaccel_agg_state();
  if (state == nullptr)
    return nullptr;

  state->key_size = sizeof(int64_t);
  state->key_type = 1;
  state->group_count = n_groups;
  state->num_aggs = 1;
  state->group_key_buf.resize(static_cast<size_t>(n_groups) * sizeof(int64_t));
  std::memcpy(state->group_key_buf.data(), out_keys,
              static_cast<size_t>(n_groups) * sizeof(int64_t));
  state->counts.reserve(n_groups);
  state->results.resize(1);
  state->results[0].reserve(n_groups);
  for (uint32_t i = 0; i < n_groups; ++i) {
    const int64_t count = static_cast<int64_t>(out_counts[i]);
    state->counts.push_back(count);
    state->results[0].push_back(static_cast<double>(count));
  }

  return state;
}

static pgaccel_agg_state* agg_count_i64_sort_reduce_device(int64_t* group_keys, size_t row_count) {
  sycl::queue* q = g_queue;
  if (q == nullptr || group_keys == nullptr || row_count == 0 ||
      row_count > static_cast<size_t>(std::numeric_limits<uint32_t>::max())) {
    return nullptr;
  }

  const pgaccel_status sort_status =
      pgaccel_sort_u64(reinterpret_cast<uint64_t*>(group_keys), row_count);
  if (sort_status != PGACCEL_OK)
    return nullptr;

  size_t out_key_bytes = 0;
  size_t out_count_bytes = 0;
  if (!checked_mul_size(row_count, sizeof(int64_t), &out_key_bytes) ||
      !checked_mul_size(row_count, sizeof(uint32_t), &out_count_bytes)) {
    return nullptr;
  }

  static constexpr size_t COUNT_COMPACT_BLOCK_ROWS = 256;
  const size_t num_blocks = (row_count + COUNT_COMPACT_BLOCK_ROWS - 1) / COUNT_COMPACT_BLOCK_ROWS;
  size_t block_bytes = 0;
  if (!checked_mul_size(num_blocks, sizeof(uint32_t), &block_bytes))
    return nullptr;

  const size_t counts_off = out_key_bytes;
  const size_t block_counts_off = counts_off + out_count_bytes;
  const size_t block_offsets_off = block_counts_off + block_bytes;
  const size_t slab_bytes = block_offsets_off + block_bytes;

  uint8_t* d_slab = sycl::malloc_shared<uint8_t>(slab_bytes, *q);
  if (d_slab == nullptr)
    return nullptr;

  auto cleanup = [&]() {
    if (d_slab)
      sycl::free(d_slab, *q);
  };

  auto* block_counts = reinterpret_cast<uint32_t*>(d_slab + block_counts_off);
  auto* block_offsets = reinterpret_cast<uint32_t*>(d_slab + block_offsets_off);

  try {
    int64_t* keys_ptr = group_keys;
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
         if (i < rows && (i == 0 || keys_ptr[i - 1] != keys_ptr[i]))
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

  uint32_t n_groups = 0;
  for (size_t block = 0; block < num_blocks; ++block) {
    block_offsets[block] = n_groups;
    const uint32_t starts = block_counts[block];
    if (static_cast<size_t>(n_groups) + static_cast<size_t>(starts) > row_count) {
      cleanup();
      return nullptr;
    }
    n_groups += starts;
  }
  if (n_groups == 0 || static_cast<size_t>(n_groups) > row_count) {
    cleanup();
    return nullptr;
  }

  try {
    int64_t* keys_ptr = group_keys;
    auto* out_keys = reinterpret_cast<int64_t*>(d_slab);
    auto* out_counts = reinterpret_cast<uint32_t*>(d_slab + counts_off);
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
         if (i < rows && (i == 0 || keys_ptr[i - 1] != keys_ptr[i]))
           is_start = 1;
         starts[lid] = is_start;
         sycl::group_barrier(it.get_group());

         if (is_start == 0)
           return;

         uint32_t local_group = 0;
         for (size_t j = 0; j < lid; ++j)
           local_group += starts[j];

         const int64_t key = keys_ptr[i];
         uint32_t count = 1;
         size_t j = i + 1;
         while (j < rows && keys_ptr[j] == key) {
           ++count;
           ++j;
         }

         const uint32_t out_idx = offsets_by_block[block] + local_group;
         out_keys[out_idx] = key;
         out_counts[out_idx] = count;
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

  const auto* out_keys = reinterpret_cast<const int64_t*>(d_slab);
  const auto* out_counts = reinterpret_cast<const uint32_t*>(d_slab + counts_off);
  pgaccel_agg_state* state = build_count_i64_state_from_u32(out_keys, out_counts, n_groups);
  cleanup();
  return state;
}

}  // namespace

// ---------------------------------------------------------------------------
// Legacy hash-based grouped aggregation (host-side group assignment + GPU
// accumulation kernel).
// ---------------------------------------------------------------------------

static pgaccel_agg_state* agg_hash(const void* group_keys, const uint8_t* group_null_mask,
                                   size_t row_count, int key_type, const void* const* value_cols,
                                   const uint8_t* const* value_nulls, const int* value_types,
                                   const pgaccel_agg_col* agg_cols, size_t num_aggs) {
  sycl::queue* q = g_queue;
  if (q == nullptr)
    return nullptr;

  // -- Phase 1 (host orchestration): assign every row to a group via hash
  // table. Hash collisions are resolved via byte-equality on the key
  // bytes. Group order matches first-occurrence order.
  std::unordered_map<uint64_t, std::vector<size_t>> hash_to_groups;
  std::vector<uint8_t> group_key_buf;
  size_t ksz = key_size_for_type(key_type);
  if (ksz == 0)
    return nullptr;

  size_t* row_to_group = sycl::malloc_shared<size_t>(row_count, *q);
  if (row_to_group == nullptr)
    return nullptr;

  size_t n_groups = 0;
  size_t null_group_idx = SIZE_MAX;
  for (size_t r = 0; r < row_count; ++r) {
    bool is_null = (group_null_mask != nullptr && group_null_mask[r]);
    if (is_null) {
      if (null_group_idx == SIZE_MAX) {
        null_group_idx = n_groups++;
        row_to_group[r] = null_group_idx;
        group_key_buf.resize(group_key_buf.size() + ksz, 0);
      } else {
        row_to_group[r] = null_group_idx;
      }
      continue;
    }

    uint64_t h = read_key_u64(group_keys, r, key_type);
    auto it = hash_to_groups.find(h);
    bool found = false;
    if (it != hash_to_groups.end()) {
      for (size_t gidx : it->second) {
        const uint8_t* stored = group_key_buf.data() + gidx * ksz;
        const uint8_t* current = static_cast<const uint8_t*>(group_keys) + r * ksz;
        if (key_bytes_equal(stored, current, ksz, key_type)) {
          row_to_group[r] = gidx;
          found = true;
          break;
        }
      }
    }

    if (!found) {
      size_t gidx = n_groups++;
      hash_to_groups[h].push_back(gidx);
      row_to_group[r] = gidx;
      const uint8_t* kb = static_cast<const uint8_t*>(group_keys) + r * ksz;
      group_key_buf.insert(group_key_buf.end(), kb, kb + ksz);
    }
  }

  if (row_count >= SORT_AGG_MIN_ROWS && n_groups > HASH_AGG_MAX_LARGE_UNSORTED_GROUPS) {
    std::fprintf(stderr,
                 "pgaccel: hash_agg large unsorted fallback rejected "
                 "(rows=%zu groups=%zu)\n",
                 row_count, n_groups);
    sycl::free(row_to_group, *q);
    return nullptr;
  }

  // -- Phase 2 (SYCL kernel): per-group accumulation. Stage value
  // columns into shared memory and dispatch one work-item per group.
  StagedColumnArrays* sc =
      stage_columns(*q, row_count, num_aggs, value_cols, value_nulls, value_types, agg_cols);
  if (sc == nullptr) {
    sycl::free(row_to_group, *q);
    return nullptr;
  }

  size_t result_count = 0;
  if (!checked_mul_size(num_aggs, n_groups, &result_count)) {
    free_staged_columns(*q, sc, num_aggs);
    sycl::free(row_to_group, *q);
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
    sycl::free(row_to_group, *q);
    return nullptr;
  }

  if (!run_unsorted_accum_kernel(
          *q, n_groups, row_count, num_aggs, row_to_group, sc->d_value_data, sc->value_data_bytes,
          sc->d_null_data, sc->null_data_bytes, sc->d_value_offsets, sc->d_null_offsets,
          sc->d_null_present, sc->d_value_types, sc->d_funcs, sc->d_col_idx, d_results, d_counts)) {
    sycl::free(d_results, *q);
    sycl::free(d_counts, *q);
    free_staged_columns(*q, sc, num_aggs);
    sycl::free(row_to_group, *q);
    return nullptr;
  }

  // -- Phase 3: build the result state.
  auto* state = new (std::nothrow) pgaccel_agg_state();
  if (state == nullptr) {
    sycl::free(d_results, *q);
    sycl::free(d_counts, *q);
    free_staged_columns(*q, sc, num_aggs);
    sycl::free(row_to_group, *q);
    return nullptr;
  }
  state->group_key_buf = std::move(group_key_buf);
  state->key_size = ksz;
  state->key_type = key_type;
  state->group_count = n_groups;
  state->num_aggs = num_aggs;
  state->counts.assign(d_counts, d_counts + n_groups);
  state->results.resize(num_aggs);
  for (size_t a = 0; a < num_aggs; ++a) {
    state->results[a].assign(d_results + a * n_groups, d_results + (a + 1) * n_groups);
  }

  sycl::free(d_results, *q);
  sycl::free(d_counts, *q);
  free_staged_columns(*q, sc, num_aggs);
  sycl::free(row_to_group, *q);
  pgaccel_record_gpu_exec();
  return state;
}

// ---------------------------------------------------------------------------
// Sort-based grouped aggregation (GPU sort + SYCL per-group reduce).
// ---------------------------------------------------------------------------

static pgaccel_agg_state* agg_sort_based(const void* group_keys, const uint8_t* group_null_mask,
                                         size_t row_count, int key_type,
                                         const void* const* value_cols,
                                         const uint8_t* const* value_nulls, const int* value_types,
                                         const pgaccel_agg_col* agg_cols, size_t num_aggs) {
  sycl::queue* q = g_queue;
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
// Mirrors agg_hash / agg_sort_based but calls the partial kernel and
// stores per-agg per-group transition states with widths matching
// pgaccel_agg_partial_width. The phase-1 grouping logic (hash table or
// GPU sort + boundary scan) is identical to finalize-mode.
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

static pgaccel_agg_state*
agg_hash_partial(const void* group_keys, const uint8_t* group_null_mask, size_t row_count,
                 int key_type, const void* const* value_cols, const uint8_t* const* value_nulls,
                 const int* value_types, const pgaccel_agg_col* agg_cols, size_t num_aggs) {
  sycl::queue* q = g_queue;
  if (q == nullptr)
    return nullptr;

  std::unordered_map<uint64_t, std::vector<size_t>> hash_to_groups;
  std::vector<uint8_t> group_key_buf;
  size_t ksz = key_size_for_type(key_type);
  if (ksz == 0)
    return nullptr;

  size_t* row_to_group = sycl::malloc_shared<size_t>(row_count, *q);
  if (row_to_group == nullptr)
    return nullptr;

  size_t n_groups = 0;
  size_t null_group_idx = SIZE_MAX;
  for (size_t r = 0; r < row_count; ++r) {
    bool is_null = (group_null_mask != nullptr && group_null_mask[r]);
    if (is_null) {
      if (null_group_idx == SIZE_MAX) {
        null_group_idx = n_groups++;
        row_to_group[r] = null_group_idx;
        group_key_buf.resize(group_key_buf.size() + ksz, 0);
      } else {
        row_to_group[r] = null_group_idx;
      }
      continue;
    }

    uint64_t h = read_key_u64(group_keys, r, key_type);
    auto it = hash_to_groups.find(h);
    bool found = false;
    if (it != hash_to_groups.end()) {
      for (size_t gidx : it->second) {
        const uint8_t* stored = group_key_buf.data() + gidx * ksz;
        const uint8_t* current = static_cast<const uint8_t*>(group_keys) + r * ksz;
        if (key_bytes_equal(stored, current, ksz, key_type)) {
          row_to_group[r] = gidx;
          found = true;
          break;
        }
      }
    }

    if (!found) {
      size_t gidx = n_groups++;
      hash_to_groups[h].push_back(gidx);
      row_to_group[r] = gidx;
      const uint8_t* kb = static_cast<const uint8_t*>(group_keys) + r * ksz;
      group_key_buf.insert(group_key_buf.end(), kb, kb + ksz);
    }
  }

  if (row_count >= SORT_AGG_MIN_ROWS && n_groups > HASH_AGG_MAX_LARGE_UNSORTED_GROUPS) {
    std::fprintf(stderr,
                 "pgaccel: hash_agg partial large unsorted fallback rejected "
                 "(rows=%zu groups=%zu)\n",
                 row_count, n_groups);
    sycl::free(row_to_group, *q);
    return nullptr;
  }

  // Per-agg width / offset layout.
  std::vector<size_t> widths_h, offsets_h;
  size_t total = 0;
  if (!compute_partial_layout(agg_cols, num_aggs, n_groups, widths_h, offsets_h, total)) {
    sycl::free(row_to_group, *q);
    return nullptr;
  }

  StagedColumnArrays* sc =
      stage_columns(*q, row_count, num_aggs, value_cols, value_nulls, value_types, agg_cols);
  if (sc == nullptr) {
    sycl::free(row_to_group, *q);
    return nullptr;
  }

  size_t* d_widths = sycl::malloc_shared<size_t>(num_aggs, *q);
  size_t* d_offsets = sycl::malloc_shared<size_t>(num_aggs, *q);
  // Allocate at least 1 element to keep pointer non-null when total == 0.
  double* d_partials = sycl::malloc_shared<double>(total > 0 ? total : 1, *q);
  int64_t* d_counts = sycl::malloc_shared<int64_t>(n_groups > 0 ? n_groups : 1, *q);
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
    sycl::free(row_to_group, *q);
    return nullptr;
  }
  for (size_t a = 0; a < num_aggs; ++a) {
    d_widths[a] = widths_h[a];
    d_offsets[a] = offsets_h[a];
  }

  if (n_groups > 0) {
    if (!run_unsorted_partial_kernel(
            *q, n_groups, row_count, num_aggs, row_to_group, sc->d_value_data, sc->value_data_bytes,
            sc->d_null_data, sc->null_data_bytes, sc->d_value_offsets, sc->d_null_offsets,
            sc->d_null_present, sc->d_value_types, sc->d_funcs, sc->d_col_idx, d_offsets, d_widths,
            d_partials, d_counts, total)) {
      sycl::free(d_widths, *q);
      sycl::free(d_offsets, *q);
      sycl::free(d_partials, *q);
      sycl::free(d_counts, *q);
      free_staged_columns(*q, sc, num_aggs);
      sycl::free(row_to_group, *q);
      return nullptr;
    }
  }

  auto* state = new (std::nothrow) pgaccel_agg_state();
  if (state == nullptr) {
    sycl::free(d_widths, *q);
    sycl::free(d_offsets, *q);
    sycl::free(d_partials, *q);
    sycl::free(d_counts, *q);
    free_staged_columns(*q, sc, num_aggs);
    sycl::free(row_to_group, *q);
    return nullptr;
  }
  state->group_key_buf = std::move(group_key_buf);
  state->key_size = ksz;
  state->key_type = key_type;
  state->group_count = n_groups;
  state->num_aggs = num_aggs;
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
  sycl::free(row_to_group, *q);
  pgaccel_record_gpu_exec();
  return state;
}

static pgaccel_agg_state* agg_sort_based_partial(const void* group_keys,
                                                 const uint8_t* group_null_mask, size_t row_count,
                                                 int key_type, const void* const* value_cols,
                                                 const uint8_t* const* value_nulls,
                                                 const int* value_types,
                                                 const pgaccel_agg_col* agg_cols, size_t num_aggs) {
  sycl::queue* q = g_queue;
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

// ===========================================================================
// Public C API
// ===========================================================================

extern "C" {

pgaccel_agg_state* pgaccel_hash_agg_execute(const void* group_keys, const uint8_t* group_null_mask,
                                            size_t row_count, int key_type,
                                            const void* const* value_cols,
                                            const uint8_t* const* value_nulls,
                                            const int* value_types, const pgaccel_agg_col* agg_cols,
                                            size_t num_aggs) {
  if (!validate_hashagg_inputs(group_keys, row_count, key_type, value_cols, value_types, agg_cols,
                               num_aggs, false))
    return nullptr;

  // Use sort-based path for large datasets except on Metal, where the
  // AdaptiveCpp backend can abort the process while validating sort/hashagg
  // argument buffers. High-cardinality large batches return unsupported.
  try {
    if (row_parallel_hashagg_supported(key_type, row_count)) {
      pgaccel_agg_state* st =
          agg_hash_row_parallel(group_keys, group_null_mask, row_count, key_type, value_cols,
                                value_nulls, value_types, agg_cols, num_aggs);
      if (st != nullptr)
        return st;
    }

    if (row_count >= SORT_AGG_MIN_ROWS && hashagg_sort_based_available()) {
      pgaccel_agg_state* st =
          agg_sort_based(group_keys, group_null_mask, row_count, key_type, value_cols, value_nulls,
                         value_types, agg_cols, num_aggs);
      if (st != nullptr)
        return st;
    }

    return agg_hash(group_keys, group_null_mask, row_count, key_type, value_cols, value_nulls,
                    value_types, agg_cols, num_aggs);
  } catch (const std::exception& e) {
    std::fprintf(stderr, "pgaccel: hash_agg_execute failed: %s\n", e.what());
  } catch (...) {
    std::fprintf(stderr, "pgaccel: hash_agg_execute failed (unknown)\n");
  }
  return nullptr;
}

pgaccel_agg_state* pgaccel_hash_count_i64_execute(const int64_t* group_keys,
                                                  const uint8_t* group_null_mask,
                                                  size_t row_count) {
  if (group_keys == nullptr || row_count == 0 ||
      row_count > static_cast<size_t>(std::numeric_limits<uint32_t>::max())) {
    return nullptr;
  }

  try {
    if (!hashagg_metal_backend()) {
      if (pgaccel_agg_state* hashed = agg_count_i64_hash(group_keys, group_null_mask, row_count))
        return hashed;
    }

    std::vector<int64_t> sorted;
    sorted.reserve(row_count);
    int64_t null_count = 0;
    for (size_t i = 0; i < row_count; ++i) {
      if (group_null_mask != nullptr && group_null_mask[i] != 0) {
        null_count += 1;
      } else {
        sorted.push_back(group_keys[i]);
      }
    }

    if (!sorted.empty()) {
      const pgaccel_status sort_status = pgaccel_sort_i64(sorted.data(), sorted.size());
      if (sort_status != PGACCEL_OK)
        return nullptr;
    }

    std::vector<int64_t> keys;
    std::vector<int64_t> counts;
    keys.reserve(sorted.size() + (null_count > 0 ? 1 : 0));
    counts.reserve(keys.capacity());

    for (size_t i = 0; i < sorted.size();) {
      const int64_t key = sorted[i];
      size_t j = i + 1;
      while (j < sorted.size() && sorted[j] == key)
        ++j;
      keys.push_back(key);
      counts.push_back(static_cast<int64_t>(j - i));
      i = j;
    }
    if (null_count > 0) {
      keys.push_back(0);
      counts.push_back(null_count);
    }

    auto* state = new (std::nothrow) pgaccel_agg_state();
    if (state == nullptr)
      return nullptr;
    state->key_size = sizeof(int64_t);
    state->key_type = 1;
    state->group_count = keys.size();
    state->num_aggs = 1;
    state->group_key_buf.resize(keys.size() * sizeof(int64_t));
    if (!keys.empty()) {
      std::memcpy(state->group_key_buf.data(), keys.data(), keys.size() * sizeof(int64_t));
    }
    state->counts = std::move(counts);
    state->results.resize(1);
    state->results[0].reserve(state->counts.size());
    for (int64_t count : state->counts) {
      state->results[0].push_back(static_cast<double>(count));
    }
    return state;
  } catch (const std::exception& e) {
    std::fprintf(stderr, "pgaccel: hash_count_i64_execute failed: %s\n", e.what());
  } catch (...) {
    std::fprintf(stderr, "pgaccel: hash_count_i64_execute failed (unknown)\n");
  }
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

pgaccel_status pgaccel_hash_agg_execute_sort_based(
    const void* group_keys, const uint8_t* group_null_mask, size_t row_count, int key_type,
    const void* const* value_cols, const uint8_t* const* value_nulls, const int* value_types,
    const pgaccel_agg_col* agg_cols, size_t num_aggs, pgaccel_agg_state** out_state) {
  if (out_state == nullptr)
    return PGACCEL_ERROR;
  *out_state = nullptr;

  if (!validate_hashagg_inputs(group_keys, row_count, key_type, value_cols, value_types, agg_cols,
                               num_aggs, false))
    return PGACCEL_ERROR;

  if (!hashagg_sort_based_available())
    return PGACCEL_UNSUPPORTED;

  try {
    pgaccel_agg_state* state =
        agg_sort_based(group_keys, group_null_mask, row_count, key_type, value_cols, value_nulls,
                       value_types, agg_cols, num_aggs);
    if (state == nullptr)
      return PGACCEL_ERROR_NO_DEVICE;
    *out_state = state;
    return PGACCEL_OK;
  } catch (const std::exception& e) {
    std::fprintf(stderr, "pgaccel: hash_agg_execute_sort_based failed: %s\n", e.what());
  } catch (...) {
    std::fprintf(stderr, "pgaccel: hash_agg_execute_sort_based failed (unknown)\n");
  }
  return PGACCEL_ERROR_NO_DEVICE;
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

pgaccel_agg_state*
pgaccel_hash_agg_execute_partial(const void* group_keys, const uint8_t* group_null_mask,
                                 size_t row_count, int key_type, const void* const* value_cols,
                                 const uint8_t* const* value_nulls, const int* value_types,
                                 const pgaccel_agg_col* agg_cols, size_t num_aggs) {
  if (!validate_hashagg_inputs(group_keys, row_count, key_type, value_cols, value_types, agg_cols,
                               num_aggs, true))
    return nullptr;

  try {
    if (row_count >= SORT_AGG_MIN_ROWS && hashagg_sort_based_available()) {
      pgaccel_agg_state* st =
          agg_sort_based_partial(group_keys, group_null_mask, row_count, key_type, value_cols,
                                 value_nulls, value_types, agg_cols, num_aggs);
      if (st != nullptr)
        return st;
    }

    return agg_hash_partial(group_keys, group_null_mask, row_count, key_type, value_cols,
                            value_nulls, value_types, agg_cols, num_aggs);
  } catch (const std::exception& e) {
    std::fprintf(stderr, "pgaccel: hash_agg_execute_partial failed: %s\n", e.what());
  } catch (...) {
    std::fprintf(stderr, "pgaccel: hash_agg_execute_partial failed (unknown)\n");
  }
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

void pgaccel_agg_free(pgaccel_agg_state* state) {
  delete state;
}

}  // extern "C"

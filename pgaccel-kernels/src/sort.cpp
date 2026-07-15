#include <sycl/sycl.hpp>

#include <algorithm>
#include <cmath>
#include <cstdint>
#include <cstdio>
#include <cstring>
#include <limits>
#include <type_traits>

#include "pgaccel_ffi.h"
#include "pgaccel_queue.h"

// ---------------------------------------------------------------------------
// Helpers
// ---------------------------------------------------------------------------

/// PG-compatible float comparison: NaN is treated as the largest value.
/// NaN == NaN is true (equal), NaN > everything else.
template <typename T>
static inline bool pg_float_less(T a, T b) {
  // NaN != NaN under IEEE 754, so (x != x) detects NaN.
  const bool a_nan = (a != a);
  const bool b_nan = (b != b);
  if (a_nan)
    return false;  // NaN is not less than anything
  if (b_nan)
    return true;  // everything is less than NaN
  return a < b;
}

/// Round up to the next power of two. Returns n if already a power of two.
static size_t next_power_of_two(size_t n) {
  if (n <= 1)
    return 1;
  --n;
  n |= n >> 1;
  n |= n >> 2;
  n |= n >> 4;
  n |= n >> 8;
  n |= n >> 16;
  n |= n >> 32;
  return n + 1;
}

// ---------------------------------------------------------------------------
// Sortable-uint conversion (used by both SYCL and Metal radix sort)
// ---------------------------------------------------------------------------

/// Convert signed int32 to sortable uint32 (flip sign bit so negative < positive).
static inline uint32_t i32_to_sortable(int32_t v) {
  return static_cast<uint32_t>(v) ^ 0x80000000u;
}

/// Convert sortable uint32 back to signed int32.
static inline int32_t sortable_to_i32(uint32_t u) {
  return static_cast<int32_t>(u ^ 0x80000000u);
}

/// Convert float to sortable uint32 (NaN-last for PG, signed zeros equal).
static inline uint32_t f32_to_sortable(float f) {
  if (f == 0.0f)
    return 0x80000000u;
  if (f != f)
    return 0xFFC00000u;
  const uint32_t bits = sycl::bit_cast<uint32_t>(f);
  const uint32_t mask = (bits & 0x80000000u) ? 0xFFFFFFFFu : 0x80000000u;
  return bits ^ mask;
}

/// Convert sortable uint32 back to float.
static inline float sortable_to_f32(uint32_t u) {
  const uint32_t mask = (u & 0x80000000u) ? 0x80000000u : 0xFFFFFFFFu;
  return sycl::bit_cast<float>(u ^ mask);
}

/// Convert double to sortable uint64 (NaN-last for PG, signed zeros equal).
static inline uint64_t f64_to_sortable(double f) {
  if (f == 0.0)
    return 0x8000000000000000ULL;
  if (f != f)
    return 0xFFF8000000000000ULL;
  const uint64_t bits = sycl::bit_cast<uint64_t>(f);
  const uint64_t mask =
      (bits & 0x8000000000000000ULL) ? 0xFFFFFFFFFFFFFFFFULL : 0x8000000000000000ULL;
  return bits ^ mask;
}

/// Convert signed int64 to sortable uint64 (flip sign bit).
static inline uint64_t i64_to_sortable(int64_t v) {
  return static_cast<uint64_t>(v) ^ 0x8000000000000000ULL;
}

/// Convert sortable uint64 back to signed int64.
static inline int64_t sortable_to_i64(uint64_t u) {
  return static_cast<int64_t>(u ^ 0x8000000000000000ULL);
}

// ---------------------------------------------------------------------------
// SYCL bitonic sort
// ---------------------------------------------------------------------------

/// Padding sentinel: +infinity for float types, max value for integer types.
template <typename T>
static T pad_value() {
  if constexpr (std::numeric_limits<T>::has_infinity) {
    return std::numeric_limits<T>::infinity();
  } else {
    return std::numeric_limits<T>::max();
  }
}

// SAFETY: g_queue is defined in device_manager.cpp and linked into the same
// shared library.  It is written once during pgaccel_init() (single writer,
// guarded by g_initialized) and read-only thereafter.

/// Get the global SYCL queue created by pgaccel_init().
/// Returns nullptr when SYCL was not initialized or init failed.
static sycl::queue* get_queue() {
  return pgaccel_get_queue();
}

// ---------------------------------------------------------------------------
// Bitonic sort — plain (in-place, values only)
// ---------------------------------------------------------------------------

template <typename T>
static pgaccel_status sycl_bitonic_sort(T* data, size_t count) {
  sycl::queue* q = get_queue();
  if (q == nullptr) {
    return PGACCEL_ERROR_NO_DEVICE;
  }

  const size_t padded = next_power_of_two(count);

  try {
    T* d_buf = sycl::malloc_device<T>(padded, *q);
    if (d_buf == nullptr) {
      return PGACCEL_OOM;
    }

    q->memcpy(d_buf, data, count * sizeof(T)).wait_and_throw();

    // Fill padding with sentinel.
    if (padded > count) {
      const T sentinel = pad_value<T>();
      q->fill(d_buf + count, sentinel, padded - count).wait_and_throw();
    }

    // Bitonic sort network. The queue is in-order, so sequential
    // submissions execute in order without explicit per-step waits.
    size_t k = 2;
    do {
      size_t j = k / 2;
      do {
        q->parallel_for(sycl::range<1>(padded), [=](sycl::id<1> id) {
           const size_t i = id[0];
           const size_t partner = i ^ j;
           if (partner > i && partner < padded) {
             const bool ascending = ((i & k) == 0);
             const T vi = d_buf[i];
             const T vp = d_buf[partner];
             if ((ascending && pg_float_less(vp, vi)) || (!ascending && pg_float_less(vi, vp))) {
               d_buf[i] = vp;
               d_buf[partner] = vi;
             }
           }
         }).wait_and_throw();
        j /= 2;
      } while (j > 0);
      k *= 2;
    } while (k <= padded);

    // Copy sorted data back (only the original count).
    q->memcpy(data, d_buf, count * sizeof(T)).wait_and_throw();
    sycl::free(d_buf, *q);

    return PGACCEL_OK;
  } catch (const sycl::exception& e) {
    fprintf(stderr, "pgaccel: SYCL sort failed: %s\n", e.what());
    return PGACCEL_ERROR;
  } catch (const std::exception& e) {
    fprintf(stderr, "pgaccel: sort failed: %s\n", e.what());
    return PGACCEL_ERROR;
  }
}

// ---------------------------------------------------------------------------
// Bitonic sort — key-value (stable for equal keys)
// ---------------------------------------------------------------------------

template <typename K>
static pgaccel_status sycl_bitonic_sort_kv(K* keys, uint32_t* indices, size_t count) {
  sycl::queue* q = get_queue();
  if (q == nullptr) {
    return PGACCEL_ERROR_NO_DEVICE;
  }

  const size_t padded = next_power_of_two(count);

  try {
    K* d_keys = sycl::malloc_device<K>(padded, *q);
    uint32_t* d_idx = sycl::malloc_device<uint32_t>(padded, *q);
    if (d_keys == nullptr || d_idx == nullptr) {
      if (d_keys)
        sycl::free(d_keys, *q);
      if (d_idx)
        sycl::free(d_idx, *q);
      return PGACCEL_OOM;
    }

    q->memcpy(d_keys, keys, count * sizeof(K)).wait_and_throw();
    q->memcpy(d_idx, indices, count * sizeof(uint32_t)).wait_and_throw();

    // Pad keys with sentinel, indices with max uint32.
    if (padded > count) {
      const K sentinel = pad_value<K>();
      q->fill(d_keys + count, sentinel, padded - count).wait_and_throw();
      q->fill(d_idx + count, std::numeric_limits<uint32_t>::max(), padded - count).wait_and_throw();
    }

    // Bitonic sort network — stable for equal keys by using index as
    // tiebreaker. Queue is in-order: no per-step wait needed.
    size_t k = 2;
    do {
      size_t j = k / 2;
      do {
        q->parallel_for(sycl::range<1>(padded), [=](sycl::id<1> id) {
           const size_t i = id[0];
           const size_t partner = i ^ j;
           if (partner > i && partner < padded) {
             const bool ascending = ((i & k) == 0);
             const K ki = d_keys[i];
             const K kp = d_keys[partner];
             const uint32_t ii = d_idx[i];
             const uint32_t ip = d_idx[partner];

             // Compare keys with NaN-aware PG semantics;
             // break ties by original index for stability.
             const bool ki_nan = (ki != ki);
             const bool kp_nan = (kp != kp);
             // NaN-aware equality: both NaN, or both
             // non-NaN and IEEE-equal.
             const bool eq = (ki_nan && kp_nan) || (!ki_nan && !kp_nan && ki == kp);
             bool should_swap = false;
             if (ascending) {
               should_swap = pg_float_less(kp, ki) || (eq && ii > ip);
             } else {
               should_swap = pg_float_less(ki, kp) || (eq && ii < ip);
             }
             if (should_swap) {
               d_keys[i] = kp;
               d_keys[partner] = ki;
               d_idx[i] = ip;
               d_idx[partner] = ii;
             }
           }
         }).wait_and_throw();
        j /= 2;
      } while (j > 0);
      k *= 2;
    } while (k <= padded);

    q->memcpy(keys, d_keys, count * sizeof(K)).wait_and_throw();
    q->memcpy(indices, d_idx, count * sizeof(uint32_t)).wait_and_throw();

    sycl::free(d_keys, *q);
    sycl::free(d_idx, *q);

    return PGACCEL_OK;
  } catch (const sycl::exception& e) {
    fprintf(stderr, "pgaccel: SYCL kv-sort failed: %s\n", e.what());
    return PGACCEL_ERROR;
  } catch (const std::exception& e) {
    fprintf(stderr, "pgaccel: kv-sort failed: %s\n", e.what());
    return PGACCEL_ERROR;
  }
}

// ---------------------------------------------------------------------------
// LSD Radix sort — 8-bit radix, 4 passes for 32-bit keys
// ---------------------------------------------------------------------------

/// Threshold: use radix sort above this count, bitonic below.
/// Radix sort has higher constant overhead (8 kernel launches for 32-bit,
/// 16 for 64-bit) but O(n·w) complexity vs bitonic O(n·log²n). Keep this
/// low enough that OLAP resident grouped aggregates do not route 10K+ row
/// key/value sorts through a large bitonic JIT graph on Metal.
///
/// NOTE: this kernel-internal dispatch threshold is separate from the
/// planner-facing `DeviceLimits::gpu_sort_min_rows` in `src/engine/cost.rs`.
/// The planner decides whether to dispatch to GPU at all; this threshold
/// picks which GPU algorithm to run once the call reaches the kernel.
static constexpr size_t RADIX_SORT_THRESHOLD = 8192;

/// Number of bins for 8-bit radix.
static constexpr size_t RADIX_BINS = 256;

/// Work-group size for the radix histogram / scatter kernels.
/// 256 = Metal's "sweet spot" threadgroup size. Each work-group handles
/// one tile of the input.
static constexpr size_t RADIX_GROUP_SIZE = 256;

/// Number of elements each work-group processes. Larger tiles reduce
/// histogram storage but slow the per-group scatter. 256 keeps
/// histograms simple (1 bin per thread per pass possible) and fits
/// comfortably in local memory.
static constexpr size_t RADIX_TILE = RADIX_GROUP_SIZE;

/// Work-group size for the device-side histogram prefix scan. One group scans
/// up to 256 radix tiles for one bin; chunk totals are scanned separately.
static constexpr size_t RADIX_SCAN_GROUP_SIZE = 256;

struct RadixScanStorage {
  uint32_t* bin_total = nullptr;
  uint32_t* bin_base = nullptr;
  uint32_t* chunk_sums = nullptr;
  uint32_t* chunk_offsets = nullptr;
  size_t chunk_count = 0;
};

static bool allocate_radix_scan_storage(sycl::queue& q, size_t group_count,
                                        RadixScanStorage& storage) {
  storage.chunk_count = (group_count + RADIX_SCAN_GROUP_SIZE - 1) / RADIX_SCAN_GROUP_SIZE;
  storage.bin_total = sycl::malloc_device<uint32_t>(RADIX_BINS, q);
  storage.bin_base = sycl::malloc_device<uint32_t>(RADIX_BINS, q);
  storage.chunk_sums = sycl::malloc_device<uint32_t>(storage.chunk_count * RADIX_BINS, q);
  storage.chunk_offsets = sycl::malloc_device<uint32_t>(storage.chunk_count * RADIX_BINS, q);
  return storage.bin_total != nullptr && storage.bin_base != nullptr &&
         storage.chunk_sums != nullptr && storage.chunk_offsets != nullptr;
}

static void free_radix_scan_storage(sycl::queue& q, RadixScanStorage& storage) {
  if (storage.bin_total)
    sycl::free(storage.bin_total, q);
  if (storage.bin_base)
    sycl::free(storage.bin_base, q);
  if (storage.chunk_sums)
    sycl::free(storage.chunk_sums, q);
  if (storage.chunk_offsets)
    sycl::free(storage.chunk_offsets, q);
  storage = {};
}

static void scan_radix_histograms_device(sycl::queue& q, uint32_t* group_hist, size_t group_count,
                                         const RadixScanStorage& storage) {
  const size_t chunk_count = storage.chunk_count;
  uint32_t* chunk_sums = storage.chunk_sums;
  auto nd = sycl::nd_range<1>(sycl::range<1>(chunk_count * RADIX_BINS * RADIX_SCAN_GROUP_SIZE),
                              sycl::range<1>(RADIX_SCAN_GROUP_SIZE));
  q.submit([&](sycl::handler& h) {
     sycl::local_accessor<uint32_t, 1> prefix(sycl::range<1>(RADIX_SCAN_GROUP_SIZE), h);
     h.parallel_for(nd, [=](sycl::nd_item<1> it) {
       const size_t lid = it.get_local_id(0);
       const size_t linear_group = it.get_group(0);
       const size_t chunk = linear_group / RADIX_BINS;
       const size_t bin = linear_group - chunk * RADIX_BINS;
       const size_t group = chunk * RADIX_SCAN_GROUP_SIZE + lid;
       const uint32_t value = group < group_count ? group_hist[group * RADIX_BINS + bin] : 0u;

       prefix[lid] = value;
       sycl::group_barrier(it.get_group());
       for (size_t offset = 1; offset < RADIX_SCAN_GROUP_SIZE; offset <<= 1) {
         const uint32_t addend = lid >= offset ? prefix[lid - offset] : 0u;
         sycl::group_barrier(it.get_group());
         prefix[lid] += addend;
         sycl::group_barrier(it.get_group());
       }

       if (group < group_count)
         group_hist[group * RADIX_BINS + bin] = prefix[lid] - value;
       if (lid == RADIX_SCAN_GROUP_SIZE - 1)
         chunk_sums[chunk * RADIX_BINS + bin] = prefix[lid];
     });
   }).wait_and_throw();

  uint32_t* chunk_offsets = storage.chunk_offsets;
  uint32_t* bin_total = storage.bin_total;
  q.parallel_for(sycl::range<1>(RADIX_BINS), [=](sycl::id<1> id) {
     const size_t bin = id[0];
     uint32_t running = 0;
     for (size_t chunk = 0; chunk < chunk_count; ++chunk) {
       const size_t offset = chunk * RADIX_BINS + bin;
       const uint32_t chunk_sum = chunk_sums[offset];
       chunk_offsets[offset] = running;
       running += chunk_sum;
     }
     bin_total[bin] = running;
   }).wait_and_throw();

  uint32_t* bin_base = storage.bin_base;
  q.submit([&](sycl::handler& h) {
     h.single_task([=]() {
       uint32_t running = 0;
       for (size_t bin = 0; bin < RADIX_BINS; ++bin) {
         const uint32_t total = bin_total[bin];
         bin_base[bin] = running;
         running += total;
       }
     });
   }).wait_and_throw();
}

/// GPU radix sort for uint32 keys + uint32 indices (key-value variant).
///
/// Algorithm — 4 passes, each pass 8 bits:
///
///   1. Compute per-tile histograms (ngroups × 256 uints) via a
///      work-group kernel that writes to a global histogram buffer.
///      All accumulation is done in local memory with a single atomic
///      increment per thread, which is reliable on Metal (local atomics
///      don't suffer from the system-scope coherence bug).
///
///   2. Exclusive scan the global histogram on the GPU (simple 256-bin
///      prefix plus per-bin group-major scans) so resident callers never
///      expose row keys or offsets to host code.
///
///   3. Scatter kernel: each work-group re-examines its tile, computes
///      a local exclusive scan per digit (to determine the offset
///      within its own tile's contribution to each bin), and writes
///      the element to `global_base[bin] + local_rank`.  No global
///      atomics in scatter — writes are to distinct locations.
///
/// This pattern avoids AdaptiveCpp's broken global-scope atomic_ref on
/// Metal.  It uses only (a) local-memory atomics, (b) non-atomic global
/// writes to pre-computed distinct offsets.
static pgaccel_status sycl_radix_sort_kv_u32(uint32_t* keys, uint32_t* indices, size_t count,
                                             int pass_count = 4) {
  sycl::queue* q = get_queue();
  if (q == nullptr)
    return PGACCEL_UNSUPPORTED;
  if (pass_count < 1 || pass_count > 4)
    return PGACCEL_ERROR;

  const size_t ngroups = (count + RADIX_TILE - 1) / RADIX_TILE;
  const size_t padded = ngroups * RADIX_TILE;

  uint32_t* buf_keys_a = nullptr;
  uint32_t* buf_keys_b = nullptr;
  uint32_t* buf_idx_a = nullptr;
  uint32_t* buf_idx_b = nullptr;
  uint32_t* d_group_hist = nullptr;
  RadixScanStorage scan;
  auto release = [&]() {
    if (buf_keys_a)
      sycl::free(buf_keys_a, *q);
    if (buf_keys_b)
      sycl::free(buf_keys_b, *q);
    if (buf_idx_a)
      sycl::free(buf_idx_a, *q);
    if (buf_idx_b)
      sycl::free(buf_idx_b, *q);
    if (d_group_hist)
      sycl::free(d_group_hist, *q);
    free_radix_scan_storage(*q, scan);
    buf_keys_a = nullptr;
    buf_keys_b = nullptr;
    buf_idx_a = nullptr;
    buf_idx_b = nullptr;
    d_group_hist = nullptr;
  };

  try {
    auto alloc_u32 = [&](size_t n) { return sycl::malloc_device<uint32_t>(n, *q); };

    buf_keys_a = alloc_u32(padded);
    buf_keys_b = alloc_u32(padded);
    buf_idx_a = alloc_u32(padded);
    buf_idx_b = alloc_u32(padded);

    // Per-group histogram / scatter base: ngroups × 256 uints.
    d_group_hist = sycl::malloc_device<uint32_t>(ngroups * RADIX_BINS, *q);
    const bool scan_allocated = allocate_radix_scan_storage(*q, ngroups, scan);

    if (!buf_keys_a || !buf_keys_b || !buf_idx_a || !buf_idx_b || !d_group_hist ||
        !scan_allocated) {
      release();
      return PGACCEL_OOM;
    }

    // Copy input into buf_a, padding with UINT32_MAX so they sort
    // to the end.  Indices padded with UINT32_MAX as well.
    q->memcpy(buf_keys_a, keys, count * sizeof(uint32_t));
    q->memcpy(buf_idx_a, indices, count * sizeof(uint32_t));
    if (padded > count) {
      q->fill(buf_keys_a + count, std::numeric_limits<uint32_t>::max(), padded - count);
      q->fill(buf_idx_a + count, std::numeric_limits<uint32_t>::max(), padded - count);
    }
    q->wait_and_throw();

    uint32_t* src_keys = buf_keys_a;
    uint32_t* dst_keys = buf_keys_b;
    uint32_t* src_idx = buf_idx_a;
    uint32_t* dst_idx = buf_idx_b;

    // Up to 4 passes × 8 bits for 32-bit keys. Dense callers can request
    // fewer passes when all normalized keys fit in fewer low bytes.
    for (int pass = 0; pass < pass_count; ++pass) {
      const int shift = pass * 8;
      const uint32_t shift_u = static_cast<uint32_t>(shift);

      // ---- Histogram pass ------------------------------------
      // One work-group per tile; each work-group builds a
      // 256-bin histogram in local memory, then writes it out
      // to d_group_hist at offset (group_id * 256).
      q->memset(d_group_hist, 0, ngroups * RADIX_BINS * sizeof(uint32_t)).wait_and_throw();

      {
        auto nd = sycl::nd_range<1>(sycl::range<1>(ngroups * RADIX_GROUP_SIZE),
                                    sycl::range<1>(RADIX_GROUP_SIZE));
        uint32_t* group_hist_ptr = d_group_hist;
        uint32_t* src_keys_ptr = src_keys;
        const size_t padded_size = padded;

        q->submit([&](sycl::handler& h) {
           // Local histogram: 256 uints.
           sycl::local_accessor<uint32_t, 1> lhist(sycl::range<1>(RADIX_BINS), h);

           h.parallel_for(nd, [=](sycl::nd_item<1> it) {
             const size_t lid = it.get_local_id(0);
             const size_t gid = it.get_group(0);
             const size_t gsz = it.get_local_range(0);

             // Zero local histogram.
             for (size_t b = lid; b < RADIX_BINS; b += gsz) {
               lhist[b] = 0u;
             }
             sycl::group_barrier(it.get_group());

             // Each thread processes one element of its tile.
             const size_t idx = gid * RADIX_TILE + lid;
             if (idx < padded_size) {
               const uint32_t k = src_keys_ptr[idx];
               const uint32_t d = (k >> shift_u) & 0xFFu;
               sycl::atomic_ref<uint32_t, sycl::memory_order::relaxed,
                                sycl::memory_scope::work_group,
                                sycl::access::address_space::local_space>
                   ref(lhist[d]);
               ref.fetch_add(1u);
             }
             sycl::group_barrier(it.get_group());

             // Write local histogram out to global.
             for (size_t b = lid; b < RADIX_BINS; b += gsz) {
               group_hist_ptr[gid * RADIX_BINS + b] = lhist[b];
             }
           });
         }).wait_and_throw();
      }

      // ---- Device-side exclusive scan of group histograms ---
      // We want d_group_hist[g][b] = base write-offset for bin b
      // within its own tile g.  Layout:
      //
      //   final position of (tile g, bin b, local_rank r) =
      //       bin_base[b] + sum_{g' < g} hist[g'][b] + r
      //
      // where bin_base[b] = sum_{b' < b} total[b'],
      //       total[b']   = sum_g hist[g][b'].
      //
      // Compute per-chunk exclusive prefixes first, then prefix chunk totals
      // and bin totals. Scatter adds chunk/bin offsets on the fly.
      scan_radix_histograms_device(*q, d_group_hist, ngroups, scan);

      // ---- Scatter pass --------------------------------------
      // Each work-group does a stable scatter of its tile.
      // For stability we must preserve intra-tile order per bin;
      // we use a local rank via a prefix-sum pass over the
      // tile's digit stream.
      //
      // Simplest correct approach: loop the tile in lock-step
      // with a single local rank counter, using local atomic
      // increments that give sequentially-consistent local
      // ordering per thread id.
      //
      // For true stability we compute the rank with a per-thread
      // scan over the 256 elements in the tile (thread lid
      // counts how many elements with lid' < lid have the same
      // digit as element lid).
      {
        auto nd = sycl::nd_range<1>(sycl::range<1>(ngroups * RADIX_GROUP_SIZE),
                                    sycl::range<1>(RADIX_GROUP_SIZE));
        uint32_t* group_base_ptr = d_group_hist;
        uint32_t* chunk_offsets_ptr = scan.chunk_offsets;
        uint32_t* bin_base_ptr = scan.bin_base;
        uint32_t* src_keys_ptr = src_keys;
        uint32_t* src_idx_ptr = src_idx;
        uint32_t* dst_keys_ptr = dst_keys;
        uint32_t* dst_idx_ptr = dst_idx;
        const size_t padded_size = padded;

        q->submit([&](sycl::handler& h) {
           // Local scratch for the tile's digits.
           sycl::local_accessor<uint32_t, 1> ldigits(sycl::range<1>(RADIX_TILE), h);

           h.parallel_for(nd, [=](sycl::nd_item<1> it) {
             const size_t lid = it.get_local_id(0);
             const size_t gid = it.get_group(0);
             const size_t tile_start = gid * RADIX_TILE;

             // Load my element's digit into local memory.
             uint32_t my_key = 0;
             uint32_t my_idx = 0;
             uint32_t my_digit = 0;
             bool in_range = (tile_start + lid) < padded_size;
             if (in_range) {
               my_key = src_keys_ptr[tile_start + lid];
               my_idx = src_idx_ptr[tile_start + lid];
               my_digit = (my_key >> shift_u) & 0xFFu;
             } else {
               my_digit = 0xFFFFFFFFu;  // sentinel, skip scatter
             }
             ldigits[lid] = my_digit;
             sycl::group_barrier(it.get_group());

             if (my_digit != 0xFFFFFFFFu) {
               // Count elements in this tile with the
               // same digit AND smaller lid (stable rank).
               uint32_t rank = 0;
               for (size_t i = 0; i < lid; ++i) {
                 if (ldigits[i] == my_digit)
                   rank++;
               }
               const size_t chunk = gid / RADIX_SCAN_GROUP_SIZE;
               const uint32_t base = group_base_ptr[gid * RADIX_BINS + my_digit] +
                                     chunk_offsets_ptr[chunk * RADIX_BINS + my_digit] +
                                     bin_base_ptr[my_digit];
               const uint32_t pos = base + rank;
               dst_keys_ptr[pos] = my_key;
               dst_idx_ptr[pos] = my_idx;
             }
           });
         }).wait_and_throw();
      }

      std::swap(src_keys, dst_keys);
      std::swap(src_idx, dst_idx);
    }

    // After 4 passes, src_{keys,idx} hold the sorted result.
    q->memcpy(keys, src_keys, count * sizeof(uint32_t)).wait_and_throw();
    q->memcpy(indices, src_idx, count * sizeof(uint32_t)).wait_and_throw();

    release();
    return PGACCEL_OK;
  } catch (const sycl::exception& e) {
    release();
    fprintf(stderr, "pgaccel: SYCL radix sort failed: %s — falling back\n", e.what());
    return PGACCEL_UNSUPPORTED;
  } catch (const std::exception& e) {
    release();
    fprintf(stderr, "pgaccel: radix sort failed: %s — falling back\n", e.what());
    return PGACCEL_UNSUPPORTED;
  }
}

static int radix_pass_count_for_bits(uint32_t radix_bits) {
  if (radix_bits == 0)
    return 1;
  const uint32_t clamped = radix_bits > 32 ? 32 : radix_bits;
  return static_cast<int>((clamped + 7u) / 8u);
}

// ---------------------------------------------------------------------------
// 64-bit radix sort — 8 passes × 8 bits
// ---------------------------------------------------------------------------

/// GPU radix sort for uint64 keys + uint32 indices.
/// Same pattern as the u32 version but 8 passes instead of 4.
static pgaccel_status sycl_radix_sort_kv_u64(uint64_t* keys, uint32_t* indices, size_t count) {
  sycl::queue* q = get_queue();
  if (q == nullptr)
    return PGACCEL_UNSUPPORTED;

  const size_t ngroups = (count + RADIX_TILE - 1) / RADIX_TILE;
  const size_t padded = ngroups * RADIX_TILE;

  uint64_t* buf_keys_a = nullptr;
  uint64_t* buf_keys_b = nullptr;
  uint32_t* buf_idx_a = nullptr;
  uint32_t* buf_idx_b = nullptr;
  uint32_t* d_group_hist = nullptr;
  RadixScanStorage scan;
  auto release = [&]() {
    if (buf_keys_a)
      sycl::free(buf_keys_a, *q);
    if (buf_keys_b)
      sycl::free(buf_keys_b, *q);
    if (buf_idx_a)
      sycl::free(buf_idx_a, *q);
    if (buf_idx_b)
      sycl::free(buf_idx_b, *q);
    if (d_group_hist)
      sycl::free(d_group_hist, *q);
    free_radix_scan_storage(*q, scan);
    buf_keys_a = nullptr;
    buf_keys_b = nullptr;
    buf_idx_a = nullptr;
    buf_idx_b = nullptr;
    d_group_hist = nullptr;
  };

  try {
    auto alloc_u64 = [&](size_t n) { return sycl::malloc_device<uint64_t>(n, *q); };
    auto alloc_u32 = [&](size_t n) { return sycl::malloc_device<uint32_t>(n, *q); };

    buf_keys_a = alloc_u64(padded);
    buf_keys_b = alloc_u64(padded);
    buf_idx_a = alloc_u32(padded);
    buf_idx_b = alloc_u32(padded);
    d_group_hist = sycl::malloc_device<uint32_t>(ngroups * RADIX_BINS, *q);
    const bool scan_allocated = allocate_radix_scan_storage(*q, ngroups, scan);

    if (!buf_keys_a || !buf_keys_b || !buf_idx_a || !buf_idx_b || !d_group_hist ||
        !scan_allocated) {
      release();
      return PGACCEL_OOM;
    }

    q->memcpy(buf_keys_a, keys, count * sizeof(uint64_t));
    q->memcpy(buf_idx_a, indices, count * sizeof(uint32_t));
    if (padded > count) {
      q->fill(buf_keys_a + count, std::numeric_limits<uint64_t>::max(), padded - count);
      q->fill(buf_idx_a + count, std::numeric_limits<uint32_t>::max(), padded - count);
    }
    q->wait_and_throw();

    uint64_t* src_keys = buf_keys_a;
    uint64_t* dst_keys = buf_keys_b;
    uint32_t* src_idx = buf_idx_a;
    uint32_t* dst_idx = buf_idx_b;

    for (int pass = 0; pass < 8; ++pass) {
      const int shift = pass * 8;
      const uint64_t shift_u = static_cast<uint64_t>(shift);

      q->memset(d_group_hist, 0, ngroups * RADIX_BINS * sizeof(uint32_t)).wait_and_throw();

      {
        auto nd = sycl::nd_range<1>(sycl::range<1>(ngroups * RADIX_GROUP_SIZE),
                                    sycl::range<1>(RADIX_GROUP_SIZE));
        uint32_t* group_hist_ptr = d_group_hist;
        uint64_t* src_keys_ptr = src_keys;
        const size_t padded_size = padded;

        q->submit([&](sycl::handler& h) {
           sycl::local_accessor<uint32_t, 1> lhist(sycl::range<1>(RADIX_BINS), h);

           h.parallel_for(nd, [=](sycl::nd_item<1> it) {
             const size_t lid = it.get_local_id(0);
             const size_t gid = it.get_group(0);
             const size_t gsz = it.get_local_range(0);

             for (size_t b = lid; b < RADIX_BINS; b += gsz) {
               lhist[b] = 0u;
             }
             sycl::group_barrier(it.get_group());

             const size_t idx = gid * RADIX_TILE + lid;
             if (idx < padded_size) {
               const uint64_t k = src_keys_ptr[idx];
               const uint32_t d = static_cast<uint32_t>((k >> shift_u) & 0xFFULL);
               sycl::atomic_ref<uint32_t, sycl::memory_order::relaxed,
                                sycl::memory_scope::work_group,
                                sycl::access::address_space::local_space>
                   ref(lhist[d]);
               ref.fetch_add(1u);
             }
             sycl::group_barrier(it.get_group());

             for (size_t b = lid; b < RADIX_BINS; b += gsz) {
               group_hist_ptr[gid * RADIX_BINS + b] = lhist[b];
             }
           });
         }).wait_and_throw();
      }

      scan_radix_histograms_device(*q, d_group_hist, ngroups, scan);

      {
        auto nd = sycl::nd_range<1>(sycl::range<1>(ngroups * RADIX_GROUP_SIZE),
                                    sycl::range<1>(RADIX_GROUP_SIZE));
        uint32_t* group_base_ptr = d_group_hist;
        uint32_t* chunk_offsets_ptr = scan.chunk_offsets;
        uint32_t* bin_base_ptr = scan.bin_base;
        uint64_t* src_keys_ptr = src_keys;
        uint32_t* src_idx_ptr = src_idx;
        uint64_t* dst_keys_ptr = dst_keys;
        uint32_t* dst_idx_ptr = dst_idx;
        const size_t padded_size = padded;

        q->submit([&](sycl::handler& h) {
           sycl::local_accessor<uint32_t, 1> ldigits(sycl::range<1>(RADIX_TILE), h);

           h.parallel_for(nd, [=](sycl::nd_item<1> it) {
             const size_t lid = it.get_local_id(0);
             const size_t gid = it.get_group(0);
             const size_t tile_start = gid * RADIX_TILE;

             uint64_t my_key = 0;
             uint32_t my_idx = 0;
             uint32_t my_digit = 0;
             bool in_range = (tile_start + lid) < padded_size;
             if (in_range) {
               my_key = src_keys_ptr[tile_start + lid];
               my_idx = src_idx_ptr[tile_start + lid];
               my_digit = static_cast<uint32_t>((my_key >> shift_u) & 0xFFULL);
             } else {
               my_digit = 0xFFFFFFFFu;
             }
             ldigits[lid] = my_digit;
             sycl::group_barrier(it.get_group());

             if (my_digit != 0xFFFFFFFFu) {
               uint32_t rank = 0;
               for (size_t i = 0; i < lid; ++i) {
                 if (ldigits[i] == my_digit)
                   rank++;
               }
               const size_t chunk = gid / RADIX_SCAN_GROUP_SIZE;
               const uint32_t base = group_base_ptr[gid * RADIX_BINS + my_digit] +
                                     chunk_offsets_ptr[chunk * RADIX_BINS + my_digit] +
                                     bin_base_ptr[my_digit];
               const uint32_t pos = base + rank;
               dst_keys_ptr[pos] = my_key;
               dst_idx_ptr[pos] = my_idx;
             }
           });
         }).wait_and_throw();
      }

      std::swap(src_keys, dst_keys);
      std::swap(src_idx, dst_idx);
    }

    q->memcpy(keys, src_keys, count * sizeof(uint64_t)).wait_and_throw();
    q->memcpy(indices, src_idx, count * sizeof(uint32_t)).wait_and_throw();

    release();
    return PGACCEL_OK;
  } catch (const sycl::exception& e) {
    release();
    fprintf(stderr, "pgaccel: SYCL radix sort u64 failed: %s — falling back\n", e.what());
    return PGACCEL_UNSUPPORTED;
  } catch (const std::exception& e) {
    release();
    fprintf(stderr, "pgaccel: radix sort u64 failed: %s — falling back\n", e.what());
    return PGACCEL_UNSUPPORTED;
  }
}

/// GPU radix sort for uint64 keys, values only.
static pgaccel_status sycl_radix_sort_u64(uint64_t* keys, size_t count) {
  sycl::queue* q = get_queue();
  if (q == nullptr)
    return PGACCEL_UNSUPPORTED;

  const size_t ngroups = (count + RADIX_TILE - 1) / RADIX_TILE;
  const size_t padded = ngroups * RADIX_TILE;

  uint64_t* buf_keys_a = nullptr;
  uint64_t* buf_keys_b = nullptr;
  uint32_t* d_group_hist = nullptr;
  RadixScanStorage scan;
  auto release = [&]() {
    if (buf_keys_a)
      sycl::free(buf_keys_a, *q);
    if (buf_keys_b)
      sycl::free(buf_keys_b, *q);
    if (d_group_hist)
      sycl::free(d_group_hist, *q);
    free_radix_scan_storage(*q, scan);
    buf_keys_a = nullptr;
    buf_keys_b = nullptr;
    d_group_hist = nullptr;
  };

  try {
    auto alloc_u64 = [&](size_t n) { return sycl::malloc_device<uint64_t>(n, *q); };

    buf_keys_a = alloc_u64(padded);
    buf_keys_b = alloc_u64(padded);
    d_group_hist = sycl::malloc_device<uint32_t>(ngroups * RADIX_BINS, *q);
    const bool scan_allocated = allocate_radix_scan_storage(*q, ngroups, scan);

    if (!buf_keys_a || !buf_keys_b || !d_group_hist || !scan_allocated) {
      release();
      return PGACCEL_OOM;
    }

    q->memcpy(buf_keys_a, keys, count * sizeof(uint64_t));
    if (padded > count) {
      q->fill(buf_keys_a + count, std::numeric_limits<uint64_t>::max(), padded - count);
    }
    q->wait_and_throw();

    uint64_t* src_keys = buf_keys_a;
    uint64_t* dst_keys = buf_keys_b;
    for (int pass = 0; pass < 8; ++pass) {
      const int shift = pass * 8;
      const uint64_t shift_u = static_cast<uint64_t>(shift);

      q->memset(d_group_hist, 0, ngroups * RADIX_BINS * sizeof(uint32_t)).wait_and_throw();

      {
        auto nd = sycl::nd_range<1>(sycl::range<1>(ngroups * RADIX_GROUP_SIZE),
                                    sycl::range<1>(RADIX_GROUP_SIZE));
        uint32_t* group_hist_ptr = d_group_hist;
        uint64_t* src_keys_ptr = src_keys;
        const size_t padded_size = padded;

        q->submit([&](sycl::handler& h) {
           sycl::local_accessor<uint32_t, 1> lhist(sycl::range<1>(RADIX_BINS), h);

           h.parallel_for(nd, [=](sycl::nd_item<1> it) {
             const size_t lid = it.get_local_id(0);
             const size_t gid = it.get_group(0);
             const size_t gsz = it.get_local_range(0);

             for (size_t b = lid; b < RADIX_BINS; b += gsz) {
               lhist[b] = 0u;
             }
             sycl::group_barrier(it.get_group());

             const size_t idx = gid * RADIX_TILE + lid;
             if (idx < padded_size) {
               const uint64_t k = src_keys_ptr[idx];
               const uint32_t d = static_cast<uint32_t>((k >> shift_u) & 0xFFULL);
               sycl::atomic_ref<uint32_t, sycl::memory_order::relaxed,
                                sycl::memory_scope::work_group,
                                sycl::access::address_space::local_space>
                   ref(lhist[d]);
               ref.fetch_add(1u);
             }
             sycl::group_barrier(it.get_group());

             for (size_t b = lid; b < RADIX_BINS; b += gsz) {
               group_hist_ptr[gid * RADIX_BINS + b] = lhist[b];
             }
           });
         }).wait_and_throw();
      }

      scan_radix_histograms_device(*q, d_group_hist, ngroups, scan);

      {
        auto nd = sycl::nd_range<1>(sycl::range<1>(ngroups * RADIX_GROUP_SIZE),
                                    sycl::range<1>(RADIX_GROUP_SIZE));
        uint32_t* group_base_ptr = d_group_hist;
        uint32_t* chunk_offsets_ptr = scan.chunk_offsets;
        uint32_t* bin_base_ptr = scan.bin_base;
        uint64_t* src_keys_ptr = src_keys;
        uint64_t* dst_keys_ptr = dst_keys;
        const size_t padded_size = padded;

        q->submit([&](sycl::handler& h) {
           sycl::local_accessor<uint32_t, 1> ldigits(sycl::range<1>(RADIX_TILE), h);

           h.parallel_for(nd, [=](sycl::nd_item<1> it) {
             const size_t lid = it.get_local_id(0);
             const size_t gid = it.get_group(0);
             const size_t tile_start = gid * RADIX_TILE;

             uint64_t my_key = 0;
             uint32_t my_digit = 0;
             bool in_range = (tile_start + lid) < padded_size;
             if (in_range) {
               my_key = src_keys_ptr[tile_start + lid];
               my_digit = static_cast<uint32_t>((my_key >> shift_u) & 0xFFULL);
             } else {
               my_digit = 0xFFFFFFFFu;
             }
             ldigits[lid] = my_digit;
             sycl::group_barrier(it.get_group());

             if (my_digit != 0xFFFFFFFFu) {
               uint32_t rank = 0;
               for (size_t i = 0; i < lid; ++i) {
                 if (ldigits[i] == my_digit)
                   rank++;
               }
               const size_t chunk = gid / RADIX_SCAN_GROUP_SIZE;
               const uint32_t base = group_base_ptr[gid * RADIX_BINS + my_digit] +
                                     chunk_offsets_ptr[chunk * RADIX_BINS + my_digit] +
                                     bin_base_ptr[my_digit];
               const uint32_t pos = base + rank;
               dst_keys_ptr[pos] = my_key;
             }
           });
         }).wait_and_throw();
      }

      std::swap(src_keys, dst_keys);
    }

    q->memcpy(keys, src_keys, count * sizeof(uint64_t)).wait_and_throw();

    release();
    return PGACCEL_OK;
  } catch (const sycl::exception& e) {
    release();
    fprintf(stderr, "pgaccel: SYCL radix sort u64 values failed: %s — falling back\n", e.what());
    return PGACCEL_UNSUPPORTED;
  } catch (const std::exception& e) {
    release();
    fprintf(stderr, "pgaccel: radix sort u64 values failed: %s — falling back\n", e.what());
    return PGACCEL_UNSUPPORTED;
  }
}

template <typename Signed, typename Sortable>
static pgaccel_status sycl_radix_sort_signed_host(Signed* keys, uint32_t* indices, size_t count) {
  sycl::queue* q = get_queue();
  if (q == nullptr)
    return PGACCEL_UNSUPPORTED;
  if (count > std::numeric_limits<uint32_t>::max())
    return PGACCEL_ERROR;

  Sortable* d_sortable = nullptr;
  uint32_t* d_indices = nullptr;
  auto release = [&]() {
    if (d_sortable)
      sycl::free(d_sortable, *q);
    if (d_indices)
      sycl::free(d_indices, *q);
  };

  try {
    d_sortable = sycl::malloc_device<Sortable>(count, *q);
    d_indices = sycl::malloc_device<uint32_t>(count, *q);
    if (!d_sortable || !d_indices) {
      release();
      return PGACCEL_OOM;
    }

    q->memcpy(d_sortable, keys, count * sizeof(Sortable));
    if (indices)
      q->memcpy(d_indices, indices, count * sizeof(uint32_t));

    constexpr Sortable sign_bit = Sortable{1} << (sizeof(Sortable) * 8 - 1);
    const bool fill_indices = indices == nullptr;
    q->parallel_for(sycl::range<1>(count), [=](sycl::id<1> id) {
       const size_t i = id[0];
       d_sortable[i] ^= sign_bit;
       if (fill_indices)
         d_indices[i] = static_cast<uint32_t>(i);
     }).wait_and_throw();

    pgaccel_status st;
    if constexpr (std::is_same_v<Sortable, uint32_t>)
      st = sycl_radix_sort_kv_u32(d_sortable, d_indices, count);
    else if (indices)
      st = sycl_radix_sort_kv_u64(d_sortable, d_indices, count);
    else
      st = sycl_radix_sort_u64(d_sortable, count);
    if (st != PGACCEL_OK) {
      release();
      return st;
    }

    q->parallel_for(sycl::range<1>(count), [=](sycl::id<1> id) {
       d_sortable[id[0]] ^= sign_bit;
     }).wait_and_throw();
    q->memcpy(keys, d_sortable, count * sizeof(Signed));
    if (indices)
      q->memcpy(indices, d_indices, count * sizeof(uint32_t));
    q->wait_and_throw();
    release();
    return PGACCEL_OK;
  } catch (const sycl::exception& e) {
    release();
    fprintf(stderr, "pgaccel: SYCL signed radix staging failed: %s\n", e.what());
    return PGACCEL_UNSUPPORTED;
  } catch (const std::exception& e) {
    release();
    fprintf(stderr, "pgaccel: signed radix staging failed: %s\n", e.what());
    return PGACCEL_UNSUPPORTED;
  }
}

/// GPU radix sort for int64 keys + indices (key-value).
static pgaccel_status sycl_radix_sort_kv_i64(int64_t* keys, uint32_t* indices, size_t count) {
  return sycl_radix_sort_signed_host<int64_t, uint64_t>(keys, indices, count);
}

/// GPU radix sort for int64 (values only).
static pgaccel_status sycl_radix_sort_i64(int64_t* data, size_t count) {
  return sycl_radix_sort_signed_host<int64_t, uint64_t>(data, nullptr, count);
}

/// GPU radix sort for int32 keys + indices (key-value).
static pgaccel_status sycl_radix_sort_kv_i32(int32_t* keys, uint32_t* indices, size_t count) {
  return sycl_radix_sort_signed_host<int32_t, uint32_t>(keys, indices, count);
}

/// GPU radix sort for USM-resident int32 keys + indices.
static pgaccel_status sycl_radix_sort_kv_i32_device(int32_t* keys, uint32_t* indices,
                                                    size_t count) {
  sycl::queue* q = get_queue();
  if (q == nullptr)
    return PGACCEL_UNSUPPORTED;

  uint32_t* ukeys = nullptr;
  try {
    ukeys = sycl::malloc_device<uint32_t>(count, *q);
    if (ukeys == nullptr)
      return PGACCEL_OOM;

    q->parallel_for(sycl::range<1>(count), [=](sycl::id<1> id) {
       const size_t i = id[0];
       ukeys[i] = i32_to_sortable(keys[i]);
     }).wait_and_throw();

    const pgaccel_status st = sycl_radix_sort_kv_u32(ukeys, indices, count);
    if (st != PGACCEL_OK) {
      sycl::free(ukeys, *q);
      return st;
    }

    q->parallel_for(sycl::range<1>(count), [=](sycl::id<1> id) {
       const size_t i = id[0];
       keys[i] = sortable_to_i32(ukeys[i]);
     }).wait_and_throw();

    sycl::free(ukeys, *q);
    return PGACCEL_OK;
  } catch (const sycl::exception& e) {
    if (ukeys)
      sycl::free(ukeys, *q);
    fprintf(stderr, "pgaccel: SYCL resident radix sort i32 failed: %s — falling back\n", e.what());
    return PGACCEL_UNSUPPORTED;
  } catch (const std::exception& e) {
    if (ukeys)
      sycl::free(ukeys, *q);
    fprintf(stderr, "pgaccel: resident radix sort i32 failed: %s — falling back\n", e.what());
    return PGACCEL_UNSUPPORTED;
  }
}

static pgaccel_status sycl_radix_sort_kv_i32_nonnegative_device(int32_t* keys, uint32_t* indices,
                                                                size_t count, uint32_t radix_bits) {
  const int pass_count = radix_pass_count_for_bits(radix_bits);
  return sycl_radix_sort_kv_u32(reinterpret_cast<uint32_t*>(keys), indices, count, pass_count);
}

template <typename Float, typename Sortable>
static pgaccel_status sycl_radix_sort_float_host(Float* keys, uint32_t* indices, size_t count) {
  sycl::queue* q = get_queue();
  if (q == nullptr)
    return PGACCEL_UNSUPPORTED;
  if (count > std::numeric_limits<uint32_t>::max())
    return PGACCEL_ERROR;

  Float* d_original_keys = nullptr;
  uint32_t* d_original_indices = nullptr;
  Sortable* d_sortable = nullptr;
  uint32_t* d_order = nullptr;
  auto release = [&]() {
    if (d_original_keys)
      sycl::free(d_original_keys, *q);
    if (d_original_indices)
      sycl::free(d_original_indices, *q);
    if (d_sortable)
      sycl::free(d_sortable, *q);
    if (d_order)
      sycl::free(d_order, *q);
  };

  try {
    d_original_keys = sycl::malloc_device<Float>(count, *q);
    d_sortable = sycl::malloc_device<Sortable>(count, *q);
    d_order = sycl::malloc_device<uint32_t>(count, *q);
    if (indices)
      d_original_indices = sycl::malloc_device<uint32_t>(count, *q);
    if (!d_original_keys || !d_sortable || !d_order || (indices && !d_original_indices)) {
      release();
      return PGACCEL_OOM;
    }

    q->memcpy(d_original_keys, keys, count * sizeof(Float));
    if (indices)
      q->memcpy(d_original_indices, indices, count * sizeof(uint32_t));
    q->parallel_for(sycl::range<1>(count), [=](sycl::id<1> id) {
       const size_t i = id[0];
       if constexpr (std::is_same_v<Float, float>)
         d_sortable[i] = f32_to_sortable(d_original_keys[i]);
       else
         d_sortable[i] = f64_to_sortable(d_original_keys[i]);
       d_order[i] = static_cast<uint32_t>(i);
     }).wait_and_throw();

    pgaccel_status st;
    if constexpr (std::is_same_v<Sortable, uint32_t>)
      st = sycl_radix_sort_kv_u32(d_sortable, d_order, count);
    else
      st = sycl_radix_sort_kv_u64(d_sortable, d_order, count);
    if (st != PGACCEL_OK) {
      release();
      return st;
    }

    Float* d_sorted_keys = reinterpret_cast<Float*>(d_sortable);
    const bool gather_indices = indices != nullptr;
    q->parallel_for(sycl::range<1>(count), [=](sycl::id<1> id) {
       const size_t i = id[0];
       const uint32_t source = d_order[i];
       d_sorted_keys[i] = d_original_keys[source];
       if (gather_indices)
         d_order[i] = d_original_indices[source];
     }).wait_and_throw();
    q->memcpy(keys, d_sorted_keys, count * sizeof(Float));
    if (indices)
      q->memcpy(indices, d_order, count * sizeof(uint32_t));
    q->wait_and_throw();
    release();
    return PGACCEL_OK;
  } catch (const sycl::exception& e) {
    release();
    fprintf(stderr, "pgaccel: SYCL floating radix staging failed: %s\n", e.what());
    return PGACCEL_UNSUPPORTED;
  } catch (const std::exception& e) {
    release();
    fprintf(stderr, "pgaccel: floating radix staging failed: %s\n", e.what());
    return PGACCEL_UNSUPPORTED;
  }
}

/// GPU radix sort for float32 keys + indices (key-value).
static pgaccel_status sycl_radix_sort_kv_f32(float* keys, uint32_t* indices, size_t count) {
  return sycl_radix_sort_float_host<float, uint32_t>(keys, indices, count);
}

/// GPU radix sort for double keys + indices (key-value).
static pgaccel_status sycl_radix_sort_kv_f64(double* keys, uint32_t* indices, size_t count) {
  return sycl_radix_sort_float_host<double, uint64_t>(keys, indices, count);
}

/// GPU radix sort for plain int32 (values only, no indices).
static pgaccel_status sycl_radix_sort_i32(int32_t* data, size_t count) {
  return sycl_radix_sort_signed_host<int32_t, uint32_t>(data, nullptr, count);
}

/// GPU radix sort for plain float32 (values only, no indices).
static pgaccel_status sycl_radix_sort_f32(float* data, size_t count) {
  return sycl_radix_sort_float_host<float, uint32_t>(data, nullptr, count);
}

/// GPU radix sort for plain double (values only, no indices).
static pgaccel_status sycl_radix_sort_f64(double* data, size_t count) {
  return sycl_radix_sort_float_host<double, uint64_t>(data, nullptr, count);
}

static pgaccel_status sycl_radix_sort_u32(uint32_t* data, size_t count) {
  sycl::queue* q = get_queue();
  if (q == nullptr)
    return PGACCEL_UNSUPPORTED;
  if (count > std::numeric_limits<uint32_t>::max())
    return PGACCEL_ERROR;

  uint32_t* d_indices = nullptr;
  try {
    d_indices = sycl::malloc_device<uint32_t>(count, *q);
    if (!d_indices)
      return PGACCEL_OOM;
    q->parallel_for(sycl::range<1>(count), [=](sycl::id<1> id) {
       const size_t i = id[0];
       d_indices[i] = static_cast<uint32_t>(i);
     }).wait_and_throw();
    const pgaccel_status st = sycl_radix_sort_kv_u32(data, d_indices, count);
    sycl::free(d_indices, *q);
    return st;
  } catch (const sycl::exception& e) {
    if (d_indices)
      sycl::free(d_indices, *q);
    fprintf(stderr, "pgaccel: SYCL u32 radix staging failed: %s\n", e.what());
    return PGACCEL_UNSUPPORTED;
  } catch (const std::exception& e) {
    if (d_indices)
      sycl::free(d_indices, *q);
    fprintf(stderr, "pgaccel: u32 radix staging failed: %s\n", e.what());
    return PGACCEL_UNSUPPORTED;
  }
}

// ===========================================================================
// Dispatch: choose GPU radix / GPU bitonic
// ===========================================================================

/// Trait: which types can use radix sort.
template <typename T>
struct is_radix_sortable : std::false_type {};
template <>
struct is_radix_sortable<float> : std::true_type {};
template <>
struct is_radix_sortable<double> : std::true_type {};
template <>
struct is_radix_sortable<int32_t> : std::true_type {};
template <>
struct is_radix_sortable<uint32_t> : std::true_type {};
template <>
struct is_radix_sortable<int64_t> : std::true_type {};
template <>
struct is_radix_sortable<uint64_t> : std::true_type {};

template <typename T>
static pgaccel_status dispatch_sort(T* data, size_t count) {
  if (data == nullptr && count > 0)
    return PGACCEL_ERROR;
  if (count <= 1)
    return PGACCEL_OK;

  // Radix-sortable keys above RADIX_SORT_THRESHOLD: use radix sort.
  // The radix kernel uses local-memory atomics only (reliable on Metal)
  // and a work-group-per-tile scatter with device-computed per-group
  // offsets — no global atomics.
  if constexpr (is_radix_sortable<T>::value) {
    if (count >= RADIX_SORT_THRESHOLD) {
      pgaccel_status st = PGACCEL_UNSUPPORTED;
      if constexpr (std::is_same_v<T, float>) {
        st = sycl_radix_sort_f32(data, count);
      } else if constexpr (std::is_same_v<T, double>) {
        st = sycl_radix_sort_f64(data, count);
      } else if constexpr (std::is_same_v<T, int32_t>) {
        st = sycl_radix_sort_i32(data, count);
      } else if constexpr (std::is_same_v<T, uint32_t>) {
        st = sycl_radix_sort_u32(reinterpret_cast<uint32_t*>(data), count);
      } else if constexpr (std::is_same_v<T, int64_t>) {
        st = sycl_radix_sort_i64(data, count);
      } else if constexpr (std::is_same_v<T, uint64_t>) {
        st = sycl_radix_sort_u64(reinterpret_cast<uint64_t*>(data), count);
      }
      if (st == PGACCEL_OK) {
        pgaccel_record_gpu_exec();
        return st;
      }
      // Fall through to bitonic on radix failure (intra-GPU retry).
    }
  }

  // Bitonic sort uses compare-and-swap only — reliable on Metal. As the
  // last kernel in the chain its status is returned as-is (honest error,
  // not collapsed to NO_DEVICE).
  {
    pgaccel_status st = sycl_bitonic_sort(data, count);
    if (st == PGACCEL_OK)
      pgaccel_record_gpu_exec();
    return st;
  }
}

template <typename T>
static pgaccel_status dispatch_sort_fp_checked(T* data, size_t count) {
  if (data == nullptr && count > 0)
    return PGACCEL_ERROR;
  if (count <= 1)
    return PGACCEL_OK;

  // fp64 always available: native on CUDA/ROCm/Level Zero, soft-fp64 on Metal.
  return dispatch_sort(data, count);
}

template <typename K>
static pgaccel_status dispatch_sort_kv(K* keys, uint32_t* indices, size_t count) {
  if ((keys == nullptr || indices == nullptr) && count > 0) {
    return PGACCEL_ERROR;
  }
  if (count <= 1)
    return PGACCEL_OK;

  // Radix-sortable keys above RADIX_SORT_THRESHOLD: use radix sort.
  if constexpr (is_radix_sortable<K>::value) {
    if (count >= RADIX_SORT_THRESHOLD) {
      pgaccel_status st = PGACCEL_UNSUPPORTED;
      if constexpr (std::is_same_v<K, float>) {
        st = sycl_radix_sort_kv_f32(keys, indices, count);
      } else if constexpr (std::is_same_v<K, double>) {
        st = sycl_radix_sort_kv_f64(keys, indices, count);
      } else if constexpr (std::is_same_v<K, int32_t>) {
        st = sycl_radix_sort_kv_i32(keys, indices, count);
      } else if constexpr (std::is_same_v<K, uint32_t>) {
        st = sycl_radix_sort_kv_u32(reinterpret_cast<uint32_t*>(keys), indices, count);
      } else if constexpr (std::is_same_v<K, int64_t>) {
        st = sycl_radix_sort_kv_i64(keys, indices, count);
      } else if constexpr (std::is_same_v<K, uint64_t>) {
        st = sycl_radix_sort_kv_u64(reinterpret_cast<uint64_t*>(keys), indices, count);
      }
      if (st == PGACCEL_OK) {
        pgaccel_record_gpu_exec();
        return st;
      }
      // Fall through to bitonic on radix failure (intra-GPU retry).
    }
  }

  // Bitonic sort uses compare-and-swap only — reliable on Metal. Last
  // kernel in the chain: status returned as-is.
  {
    pgaccel_status st = sycl_bitonic_sort_kv(keys, indices, count);
    if (st == PGACCEL_OK)
      pgaccel_record_gpu_exec();
    return st;
  }
}

static pgaccel_status dispatch_sort_kv_i32_device(int32_t* keys, uint32_t* indices, size_t count) {
  if ((keys == nullptr || indices == nullptr) && count > 0)
    return PGACCEL_ERROR;
  if (count <= 1)
    return PGACCEL_OK;

  if (count >= RADIX_SORT_THRESHOLD) {
    const pgaccel_status st = sycl_radix_sort_kv_i32_device(keys, indices, count);
    if (st == PGACCEL_OK) {
      pgaccel_record_gpu_exec();
      return st;
    }
    // Fall through to bitonic on radix failure (intra-GPU retry).
  }

  const pgaccel_status st = sycl_bitonic_sort_kv(keys, indices, count);
  if (st == PGACCEL_OK)
    pgaccel_record_gpu_exec();
  return st;
}

static pgaccel_status dispatch_sort_kv_i32_nonnegative_device(int32_t* keys, uint32_t* indices,
                                                              size_t count, uint32_t radix_bits) {
  if ((keys == nullptr || indices == nullptr) && count > 0)
    return PGACCEL_ERROR;
  if (count <= 1)
    return PGACCEL_OK;

  if (count >= RADIX_SORT_THRESHOLD) {
    const pgaccel_status st =
        sycl_radix_sort_kv_i32_nonnegative_device(keys, indices, count, radix_bits);
    if (st == PGACCEL_OK) {
      pgaccel_record_gpu_exec();
      return st;
    }
    // Fall through to bitonic on radix failure (intra-GPU retry).
  }

  const pgaccel_status st = sycl_bitonic_sort_kv(keys, indices, count);
  if (st == PGACCEL_OK)
    pgaccel_record_gpu_exec();
  return st;
}

template <typename K>
static pgaccel_status dispatch_sort_kv_fp_checked(K* keys, uint32_t* indices, size_t count) {
  if ((keys == nullptr || indices == nullptr) && count > 0) {
    return PGACCEL_ERROR;
  }
  if (count <= 1)
    return PGACCEL_OK;

  // fp64 always available: native on CUDA/ROCm/Level Zero, soft-fp64 on Metal.
  return dispatch_sort_kv(keys, indices, count);
}

// ---------------------------------------------------------------------------
// Bounded top-k — tile-local GPU candidate selection + GPU candidate sort
// ---------------------------------------------------------------------------

static constexpr size_t TOPK_TILE = 1024;
static constexpr size_t TOPK_GROUP_SIZE = 256;

template <typename K>
static inline bool pg_key_eq(K a, K b) {
  const bool a_nan = (a != a);
  const bool b_nan = (b != b);
  return (a_nan && b_nan) || (!a_nan && !b_nan && a == b);
}

template <typename K>
static inline bool topk_order_less(K a, uint32_t ai, K b, uint32_t bi, bool largest) {
  const bool a_pad = ai == std::numeric_limits<uint32_t>::max();
  const bool b_pad = bi == std::numeric_limits<uint32_t>::max();
  if (a_pad || b_pad)
    return !a_pad && b_pad;

  if (pg_key_eq(a, b))
    return ai < bi;
  return largest ? pg_float_less(b, a) : pg_float_less(a, b);
}

static pgaccel_status sort_topk_candidates(float* keys, uint32_t* indices, size_t count,
                                           bool largest, void*& sortable_storage, sycl::queue& q) {
  auto* sortable = sycl::malloc_device<uint32_t>(count, q);
  sortable_storage = sortable;
  if (sortable == nullptr)
    return PGACCEL_OOM;
  q.parallel_for(sycl::range<1>(count), [=](sycl::id<1> id) {
     const size_t i = id[0];
     const uint32_t normalized = f32_to_sortable(keys[i]);
     sortable[i] = largest ? ~normalized : normalized;
   }).wait_and_throw();
  return sycl_radix_sort_kv_u32(sortable, indices, count);
}

static pgaccel_status sort_topk_candidates(int32_t* keys, uint32_t* indices, size_t count,
                                           bool largest, void*& sortable_storage, sycl::queue& q) {
  auto* sortable = sycl::malloc_device<uint32_t>(count, q);
  sortable_storage = sortable;
  if (sortable == nullptr)
    return PGACCEL_OOM;
  q.parallel_for(sycl::range<1>(count), [=](sycl::id<1> id) {
     const size_t i = id[0];
     const uint32_t normalized = i32_to_sortable(keys[i]);
     sortable[i] = largest ? ~normalized : normalized;
   }).wait_and_throw();
  return sycl_radix_sort_kv_u32(sortable, indices, count);
}

static pgaccel_status sort_topk_candidates(double* keys, uint32_t* indices, size_t count,
                                           bool largest, void*& sortable_storage, sycl::queue& q) {
  auto* sortable = sycl::malloc_device<uint64_t>(count, q);
  sortable_storage = sortable;
  if (sortable == nullptr)
    return PGACCEL_OOM;
  q.parallel_for(sycl::range<1>(count), [=](sycl::id<1> id) {
     const size_t i = id[0];
     const uint64_t normalized = f64_to_sortable(keys[i]);
     sortable[i] = largest ? ~normalized : normalized;
   }).wait_and_throw();
  return sycl_radix_sort_kv_u64(sortable, indices, count);
}

static pgaccel_status sort_topk_candidates(int64_t* keys, uint32_t* indices, size_t count,
                                           bool largest, void*& sortable_storage, sycl::queue& q) {
  auto* sortable = sycl::malloc_device<uint64_t>(count, q);
  sortable_storage = sortable;
  if (sortable == nullptr)
    return PGACCEL_OOM;
  q.parallel_for(sycl::range<1>(count), [=](sycl::id<1> id) {
     const size_t i = id[0];
     const uint64_t normalized = i64_to_sortable(keys[i]);
     sortable[i] = largest ? ~normalized : normalized;
   }).wait_and_throw();
  return sycl_radix_sort_kv_u64(sortable, indices, count);
}

template <typename K>
static pgaccel_status sycl_topk_device(const K* keys, size_t count, size_t k, bool largest,
                                       uint32_t* out_indices, size_t candidate_count,
                                       size_t candidate_capacity) {
  sycl::queue* q = get_queue();
  if (q == nullptr)
    return PGACCEL_UNSUPPORTED;

  const size_t num_tiles = (count + TOPK_TILE - 1) / TOPK_TILE;
  const size_t local_k = std::min(k, TOPK_TILE);
  if (candidate_capacity < num_tiles * local_k)
    return PGACCEL_ERROR;

  K* d_keys = nullptr;
  K* d_cand_keys = nullptr;
  uint32_t* d_cand_indices = nullptr;
  uint32_t* d_result_indices = nullptr;
  void* d_sortable = nullptr;
  auto release = [&]() {
    if (d_keys)
      sycl::free(d_keys, *q);
    if (d_cand_keys)
      sycl::free(d_cand_keys, *q);
    if (d_cand_indices)
      sycl::free(d_cand_indices, *q);
    if (d_result_indices)
      sycl::free(d_result_indices, *q);
    if (d_sortable)
      sycl::free(d_sortable, *q);
    d_keys = nullptr;
    d_cand_keys = nullptr;
    d_cand_indices = nullptr;
    d_result_indices = nullptr;
    d_sortable = nullptr;
  };

  try {
    d_keys = sycl::malloc_device<K>(count, *q);
    d_cand_keys = sycl::malloc_device<K>(candidate_capacity, *q);
    d_cand_indices = sycl::malloc_device<uint32_t>(candidate_capacity, *q);
    d_result_indices = sycl::malloc_device<uint32_t>(k, *q);
    if (!d_keys || !d_cand_keys || !d_cand_indices || !d_result_indices) {
      release();
      return PGACCEL_OOM;
    }

    q->memcpy(d_keys, keys, count * sizeof(K)).wait_and_throw();

    auto nd = sycl::nd_range<1>(sycl::range<1>(num_tiles * TOPK_GROUP_SIZE),
                                sycl::range<1>(TOPK_GROUP_SIZE));
    const size_t take_per_tile = local_k;
    const bool want_largest = largest;
    K* keys_ptr = d_keys;
    K* cand_keys_ptr = d_cand_keys;
    uint32_t* cand_idx_ptr = d_cand_indices;

    q->submit([&](sycl::handler& h) {
       sycl::local_accessor<K, 1> lkeys(sycl::range<1>(TOPK_TILE), h);
       sycl::local_accessor<uint32_t, 1> lidx(sycl::range<1>(TOPK_TILE), h);

       h.parallel_for(nd, [=](sycl::nd_item<1> it) {
         const size_t lid = it.get_local_id(0);
         const size_t gid = it.get_group(0);
         const size_t tile_start = gid * TOPK_TILE;
         const size_t remaining = (tile_start < count) ? (count - tile_start) : 0;
         const size_t tile_count = remaining < TOPK_TILE ? remaining : TOPK_TILE;

         for (size_t pos = lid; pos < TOPK_TILE; pos += TOPK_GROUP_SIZE) {
           const size_t global = tile_start + pos;
           if (pos < tile_count && global < count) {
             lkeys[pos] = keys_ptr[global];
             lidx[pos] = static_cast<uint32_t>(global);
           } else {
             lkeys[pos] = K{};
             lidx[pos] = std::numeric_limits<uint32_t>::max();
           }
         }
         sycl::group_barrier(it.get_group());

         // Bitonic sort the tile in local memory. Only TOPK_TILE / 2
         // compare pairs exist per step; TOPK_GROUP_SIZE threads cover
         // them in two strided iterations.
         for (size_t width = 2; width <= TOPK_TILE; width <<= 1) {
           for (size_t stride = width >> 1; stride > 0; stride >>= 1) {
             for (size_t pair = lid; pair < TOPK_TILE / 2; pair += TOPK_GROUP_SIZE) {
               const size_t low = pair & (stride - 1);
               const size_t i = ((pair - low) << 1) + low;
               const size_t j = i + stride;
               const bool ascending = (i & width) == 0;

               const K ki = lkeys[i];
               const K kj = lkeys[j];
               const uint32_t ii = lidx[i];
               const uint32_t ij = lidx[j];

               const bool swap = ascending ? topk_order_less(kj, ij, ki, ii, want_largest)
                                           : topk_order_less(ki, ii, kj, ij, want_largest);
               if (swap) {
                 lkeys[i] = kj;
                 lkeys[j] = ki;
                 lidx[i] = ij;
                 lidx[j] = ii;
               }
             }
             sycl::group_barrier(it.get_group());
           }
         }

         const size_t take = tile_count < take_per_tile ? tile_count : take_per_tile;
         const size_t cand_base = gid * take_per_tile;
         for (size_t out = lid; out < take; out += TOPK_GROUP_SIZE) {
           cand_keys_ptr[cand_base + out] = lkeys[out];
           cand_idx_ptr[cand_base + out] = lidx[out];
         }
       });
     }).wait_and_throw();

    sycl::free(d_keys, *q);
    d_keys = nullptr;

    const pgaccel_status st = sort_topk_candidates(d_cand_keys, d_cand_indices, candidate_count,
                                                   want_largest, d_sortable, *q);

    if (st == PGACCEL_OOM) {
      release();
      return PGACCEL_OOM;
    }
    if (st == PGACCEL_ERROR) {
      release();
      return PGACCEL_ERROR;
    }
    if (st != PGACCEL_OK) {
      release();
      return PGACCEL_UNSUPPORTED;
    }
    q->parallel_for(sycl::range<1>(k), [=](sycl::id<1> id) {
       const size_t i = id[0];
       d_result_indices[i] = d_cand_indices[i];
     }).wait_and_throw();
    q->memcpy(out_indices, d_result_indices, k * sizeof(uint32_t)).wait_and_throw();
    release();
    return PGACCEL_OK;
  } catch (const sycl::exception& e) {
    release();
    fprintf(stderr, "pgaccel: SYCL top-k device selection failed: %s\n", e.what());
    return PGACCEL_UNSUPPORTED;
  } catch (const std::exception& e) {
    release();
    fprintf(stderr, "pgaccel: top-k device selection failed: %s\n", e.what());
    return PGACCEL_UNSUPPORTED;
  }
}

template <typename K>
static pgaccel_status dispatch_topk_kv(const K* keys, size_t count, size_t requested_count,
                                       bool largest, uint32_t* out_indices, size_t* out_count) {
  if (out_count == nullptr)
    return PGACCEL_ERROR;
  std::memset(out_count, 0, sizeof(*out_count));
  if (requested_count == 0)
    return PGACCEL_OK;
  if (count == 0)
    return PGACCEL_OK;
  if (keys == nullptr || out_indices == nullptr)
    return PGACCEL_ERROR;
  if (count > std::numeric_limits<uint32_t>::max())
    return PGACCEL_ERROR;

  const size_t take = std::min(requested_count, count);
  const size_t num_tiles = (count + TOPK_TILE - 1) / TOPK_TILE;
  const size_t local_k = std::min(take, TOPK_TILE);
  const size_t last_tile_count = count - (num_tiles - 1) * TOPK_TILE;
  const size_t last_take = std::min(local_k, last_tile_count);
  const size_t candidate_count = (num_tiles - 1) * local_k + last_take;
  const size_t candidate_capacity = num_tiles * local_k;

  const pgaccel_status st = sycl_topk_device(keys, count, take, largest, out_indices,
                                             candidate_count, candidate_capacity);
  if (st == PGACCEL_OOM)
    return PGACCEL_OOM;
  if (st == PGACCEL_ERROR)
    return PGACCEL_ERROR;
  if (st != PGACCEL_OK)
    return PGACCEL_UNSUPPORTED;

  sycl::queue* q = get_queue();
  if (q == nullptr)
    return PGACCEL_ERROR_NO_DEVICE;
  size_t* d_result_count = sycl::malloc_device<size_t>(1, *q);
  if (d_result_count == nullptr)
    return PGACCEL_OOM;
  try {
    q->single_task([=]() { d_result_count[0] = take; }).wait_and_throw();
    q->memcpy(out_count, d_result_count, sizeof(size_t)).wait_and_throw();
    sycl::free(d_result_count, *q);
  } catch (...) {
    sycl::free(d_result_count, *q);
    throw;
  }

  pgaccel_record_gpu_exec();
  return PGACCEL_OK;
}

// ===========================================================================
// Public C API
// ===========================================================================

extern "C" {

pgaccel_status pgaccel_sort_f32(float* data, size_t count) try {
  return dispatch_sort(data, count);
} catch (const pgaccel_no_device_error&) {
  return PGACCEL_ERROR_NO_DEVICE;
} catch (const std::exception& e) {
  return pgaccel_kernel_failure("pgaccel_sort_f32", &e);
} catch (...) {
  return pgaccel_kernel_failure("pgaccel_sort_f32", nullptr);
}

pgaccel_status pgaccel_sort_f64(double* data, size_t count) try {
  return dispatch_sort_fp_checked(data, count);
} catch (const pgaccel_no_device_error&) {
  return PGACCEL_ERROR_NO_DEVICE;
} catch (const std::exception& e) {
  return pgaccel_kernel_failure("pgaccel_sort_f64", &e);
} catch (...) {
  return pgaccel_kernel_failure("pgaccel_sort_f64", nullptr);
}

pgaccel_status pgaccel_sort_i32(int32_t* data, size_t count) try {
  return dispatch_sort(data, count);
} catch (const pgaccel_no_device_error&) {
  return PGACCEL_ERROR_NO_DEVICE;
} catch (const std::exception& e) {
  return pgaccel_kernel_failure("pgaccel_sort_i32", &e);
} catch (...) {
  return pgaccel_kernel_failure("pgaccel_sort_i32", nullptr);
}

pgaccel_status pgaccel_sort_i64(int64_t* data, size_t count) try {
  return dispatch_sort(data, count);
} catch (const pgaccel_no_device_error&) {
  return PGACCEL_ERROR_NO_DEVICE;
} catch (const std::exception& e) {
  return pgaccel_kernel_failure("pgaccel_sort_i64", &e);
} catch (...) {
  return pgaccel_kernel_failure("pgaccel_sort_i64", nullptr);
}

pgaccel_status pgaccel_sort_u64(uint64_t* data, size_t count) try {
  return dispatch_sort(data, count);
} catch (const pgaccel_no_device_error&) {
  return PGACCEL_ERROR_NO_DEVICE;
} catch (const std::exception& e) {
  return pgaccel_kernel_failure("pgaccel_sort_u64", &e);
} catch (...) {
  return pgaccel_kernel_failure("pgaccel_sort_u64", nullptr);
}

pgaccel_status pgaccel_sort_kv_f32(float* keys, uint32_t* indices, size_t count) try {
  return dispatch_sort_kv(keys, indices, count);
} catch (const pgaccel_no_device_error&) {
  return PGACCEL_ERROR_NO_DEVICE;
} catch (const std::exception& e) {
  return pgaccel_kernel_failure("pgaccel_sort_kv_f32", &e);
} catch (...) {
  return pgaccel_kernel_failure("pgaccel_sort_kv_f32", nullptr);
}

pgaccel_status pgaccel_sort_kv_f64(double* keys, uint32_t* indices, size_t count) try {
  return dispatch_sort_kv_fp_checked(keys, indices, count);
} catch (const pgaccel_no_device_error&) {
  return PGACCEL_ERROR_NO_DEVICE;
} catch (const std::exception& e) {
  return pgaccel_kernel_failure("pgaccel_sort_kv_f64", &e);
} catch (...) {
  return pgaccel_kernel_failure("pgaccel_sort_kv_f64", nullptr);
}

pgaccel_status pgaccel_sort_kv_i32(int32_t* keys, uint32_t* indices, size_t count) try {
  return dispatch_sort_kv(keys, indices, count);
} catch (const pgaccel_no_device_error&) {
  return PGACCEL_ERROR_NO_DEVICE;
} catch (const std::exception& e) {
  return pgaccel_kernel_failure("pgaccel_sort_kv_i32", &e);
} catch (...) {
  return pgaccel_kernel_failure("pgaccel_sort_kv_i32", nullptr);
}

pgaccel_status pgaccel_sort_kv_i32_device(int32_t* keys, uint32_t* indices, size_t count) try {
  return dispatch_sort_kv_i32_device(keys, indices, count);
} catch (const pgaccel_no_device_error&) {
  return PGACCEL_ERROR_NO_DEVICE;
} catch (const std::exception& e) {
  return pgaccel_kernel_failure("pgaccel_sort_kv_i32_device", &e);
} catch (...) {
  return pgaccel_kernel_failure("pgaccel_sort_kv_i32_device", nullptr);
}

pgaccel_status pgaccel_sort_kv_i32_nonnegative_device(int32_t* keys, uint32_t* indices,
                                                      size_t count, uint32_t radix_bits) try {
  return dispatch_sort_kv_i32_nonnegative_device(keys, indices, count, radix_bits);
} catch (const pgaccel_no_device_error&) {
  return PGACCEL_ERROR_NO_DEVICE;
} catch (const std::exception& e) {
  return pgaccel_kernel_failure("pgaccel_sort_kv_i32_nonnegative_device", &e);
} catch (...) {
  return pgaccel_kernel_failure("pgaccel_sort_kv_i32_nonnegative_device", nullptr);
}

pgaccel_status pgaccel_sort_kv_i64(int64_t* keys, uint32_t* indices, size_t count) try {
  return dispatch_sort_kv(keys, indices, count);
} catch (const pgaccel_no_device_error&) {
  return PGACCEL_ERROR_NO_DEVICE;
} catch (const std::exception& e) {
  return pgaccel_kernel_failure("pgaccel_sort_kv_i64", &e);
} catch (...) {
  return pgaccel_kernel_failure("pgaccel_sort_kv_i64", nullptr);
}

pgaccel_status pgaccel_topk_kv_f32(const float* keys, size_t count, size_t k, uint8_t largest,
                                   uint32_t* out_indices, size_t* out_count) try {
  return dispatch_topk_kv(keys, count, k, largest != 0, out_indices, out_count);
} catch (const pgaccel_no_device_error&) {
  return PGACCEL_ERROR_NO_DEVICE;
} catch (const std::exception& e) {
  return pgaccel_kernel_failure("pgaccel_topk_kv_f32", &e);
} catch (...) {
  return pgaccel_kernel_failure("pgaccel_topk_kv_f32", nullptr);
}

pgaccel_status pgaccel_topk_kv_f64(const double* keys, size_t count, size_t k, uint8_t largest,
                                   uint32_t* out_indices, size_t* out_count) try {
  return dispatch_topk_kv(keys, count, k, largest != 0, out_indices, out_count);
} catch (const pgaccel_no_device_error&) {
  return PGACCEL_ERROR_NO_DEVICE;
} catch (const std::exception& e) {
  return pgaccel_kernel_failure("pgaccel_topk_kv_f64", &e);
} catch (...) {
  return pgaccel_kernel_failure("pgaccel_topk_kv_f64", nullptr);
}

pgaccel_status pgaccel_topk_kv_i32(const int32_t* keys, size_t count, size_t k, uint8_t largest,
                                   uint32_t* out_indices, size_t* out_count) try {
  return dispatch_topk_kv(keys, count, k, largest != 0, out_indices, out_count);
} catch (const pgaccel_no_device_error&) {
  return PGACCEL_ERROR_NO_DEVICE;
} catch (const std::exception& e) {
  return pgaccel_kernel_failure("pgaccel_topk_kv_i32", &e);
} catch (...) {
  return pgaccel_kernel_failure("pgaccel_topk_kv_i32", nullptr);
}

pgaccel_status pgaccel_topk_kv_i64(const int64_t* keys, size_t count, size_t k, uint8_t largest,
                                   uint32_t* out_indices, size_t* out_count) try {
  return dispatch_topk_kv(keys, count, k, largest != 0, out_indices, out_count);
} catch (const pgaccel_no_device_error&) {
  return PGACCEL_ERROR_NO_DEVICE;
} catch (const std::exception& e) {
  return pgaccel_kernel_failure("pgaccel_topk_kv_i64", &e);
} catch (...) {
  return pgaccel_kernel_failure("pgaccel_topk_kv_i64", nullptr);
}

}  // extern "C"

#include <sycl/sycl.hpp>

#include <algorithm>
#include <cmath>
#include <cstdint>
#include <cstdio>
#include <cstring>
#include <limits>
#include <numeric>
#include <type_traits>
#include <vector>

#include "pgaccel_ffi.h"

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
    f = 0.0f;
  else if (f != f)
    f = std::numeric_limits<float>::quiet_NaN();
  uint32_t bits;
  std::memcpy(&bits, &f, sizeof(bits));
  uint32_t mask = (bits & 0x80000000u) ? 0xFFFFFFFFu : 0x80000000u;
  return bits ^ mask;
}

/// Convert sortable uint32 back to float.
static inline float sortable_to_f32(uint32_t u) {
  uint32_t mask = (u & 0x80000000u) ? 0x80000000u : 0xFFFFFFFFu;
  uint32_t bits = u ^ mask;
  float f;
  std::memcpy(&f, &bits, sizeof(f));
  return f;
}

/// Convert double to sortable uint64 (NaN-last for PG, signed zeros equal).
static inline uint64_t f64_to_sortable(double f) {
  if (f == 0.0)
    f = 0.0;
  else if (f != f)
    f = std::numeric_limits<double>::quiet_NaN();
  uint64_t bits;
  std::memcpy(&bits, &f, sizeof(bits));
  uint64_t mask = (bits & 0x8000000000000000ULL) ? 0xFFFFFFFFFFFFFFFFULL : 0x8000000000000000ULL;
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
extern sycl::queue* g_queue;

/// Get the global SYCL queue created by pgaccel_init().
/// Returns nullptr when SYCL was not initialized or init failed.
static sycl::queue* get_queue() {
  return g_queue;
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
    for (size_t k = 2; k <= padded; k *= 2) {
      for (size_t j = k / 2; j > 0; j /= 2) {
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
        });
      }
    }
    // Single wait after all bitonic steps complete.
    q->wait_and_throw();

    // Copy sorted data back (only the original count).
    q->memcpy(data, d_buf, count * sizeof(T)).wait_and_throw();
    sycl::free(d_buf, *q);

    return PGACCEL_OK;
  } catch (const sycl::exception& e) {
    fprintf(stderr, "pgaccel: SYCL sort failed: %s\n", e.what());
    return PGACCEL_ERROR_NO_DEVICE;
  } catch (const std::exception& e) {
    fprintf(stderr, "pgaccel: sort failed: %s\n", e.what());
    return PGACCEL_ERROR_NO_DEVICE;
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
    for (size_t k = 2; k <= padded; k *= 2) {
      for (size_t j = k / 2; j > 0; j /= 2) {
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
        });
      }
    }
    // Single wait after all bitonic steps complete.
    q->wait_and_throw();

    q->memcpy(keys, d_keys, count * sizeof(K)).wait_and_throw();
    q->memcpy(indices, d_idx, count * sizeof(uint32_t)).wait_and_throw();

    sycl::free(d_keys, *q);
    sycl::free(d_idx, *q);

    return PGACCEL_OK;
  } catch (const sycl::exception& e) {
    fprintf(stderr, "pgaccel: SYCL kv-sort failed: %s\n", e.what());
    return PGACCEL_ERROR_NO_DEVICE;
  } catch (const std::exception& e) {
    fprintf(stderr, "pgaccel: kv-sort failed: %s\n", e.what());
    return PGACCEL_ERROR_NO_DEVICE;
  }
}

// ---------------------------------------------------------------------------
// LSD Radix sort — 8-bit radix, 4 passes for 32-bit keys
// ---------------------------------------------------------------------------

/// Threshold: use radix sort above this count, bitonic below.
/// Radix sort has higher constant overhead (8 kernel launches for 32-bit,
/// 16 for 64-bit) but O(n·w) complexity vs bitonic O(n·log²n). For integer
/// keys above ~100k rows, radix wins decisively.
///
/// NOTE: this kernel-internal dispatch threshold is separate from the
/// planner-facing `DeviceLimits::gpu_sort_min_rows` in `src/engine/cost.rs`.
/// The planner decides whether to dispatch to GPU at all; this threshold
/// picks which GPU algorithm to run once the call reaches the kernel.
static constexpr size_t RADIX_SORT_THRESHOLD = 65536;

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
///   2. Exclusive scan the global histogram on the host (simple 256-bin
///      prefix per group, computed over the group-major layout so we
///      can hand each work-group its exact per-bin base offset).
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
static pgaccel_status sycl_radix_sort_kv_u32(uint32_t* keys, uint32_t* indices, size_t count) {
  sycl::queue* q = get_queue();
  if (q == nullptr)
    return PGACCEL_UNSUPPORTED;

  const size_t ngroups = (count + RADIX_TILE - 1) / RADIX_TILE;
  const size_t padded = ngroups * RADIX_TILE;

  try {
    auto alloc_u32 = [&](size_t n) { return sycl::malloc_device<uint32_t>(n, *q); };

    uint32_t* buf_keys_a = alloc_u32(padded);
    uint32_t* buf_keys_b = alloc_u32(padded);
    uint32_t* buf_idx_a = alloc_u32(padded);
    uint32_t* buf_idx_b = alloc_u32(padded);

    // Per-group histogram: ngroups × 256 uints.
    // Must be shared so host can do the inter-group prefix scan.
    uint32_t* d_group_hist = sycl::malloc_shared<uint32_t>(ngroups * RADIX_BINS, *q);

    if (!buf_keys_a || !buf_keys_b || !buf_idx_a || !buf_idx_b || !d_group_hist) {
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

    // Scratch for host-side scan: per-group per-bin base offsets.
    // Same layout as d_group_hist but holds the exclusive scan.
    std::vector<uint32_t> scan_buf(ngroups * RADIX_BINS);

    // 4 passes × 8 bits for 32-bit keys.
    for (int pass = 0; pass < 4; ++pass) {
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

      // ---- Host-side exclusive scan of group histograms -----
      // We want scan_buf[g][b] = base write-offset for bin b
      // within its own tile g.  Layout:
      //
      //   final position of (tile g, bin b, local_rank r) =
      //       bin_base[b] + sum_{g' < g} hist[g'][b] + r
      //
      // where bin_base[b] = sum_{b' < b} total[b'],
      //       total[b']   = sum_g hist[g][b'].
      //
      // Compute total[b] first, then prefix -> bin_base,
      // then fill scan_buf via per-bin running accumulator.
      uint32_t bin_total[RADIX_BINS];
      for (size_t b = 0; b < RADIX_BINS; ++b)
        bin_total[b] = 0;
      for (size_t g = 0; g < ngroups; ++g) {
        for (size_t b = 0; b < RADIX_BINS; ++b) {
          bin_total[b] += d_group_hist[g * RADIX_BINS + b];
        }
      }
      uint32_t bin_base[RADIX_BINS];
      bin_base[0] = 0;
      for (size_t b = 1; b < RADIX_BINS; ++b) {
        bin_base[b] = bin_base[b - 1] + bin_total[b - 1];
      }
      // Per-group base = bin_base + running sum of hist[g'][b].
      uint32_t running[RADIX_BINS];
      for (size_t b = 0; b < RADIX_BINS; ++b)
        running[b] = bin_base[b];
      for (size_t g = 0; g < ngroups; ++g) {
        for (size_t b = 0; b < RADIX_BINS; ++b) {
          scan_buf[g * RADIX_BINS + b] = running[b];
          running[b] += d_group_hist[g * RADIX_BINS + b];
        }
      }
      q->memcpy(d_group_hist, scan_buf.data(), ngroups * RADIX_BINS * sizeof(uint32_t))
          .wait_and_throw();

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
               const uint32_t base = group_base_ptr[gid * RADIX_BINS + my_digit];
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

    sycl::free(d_group_hist, *q);
    sycl::free(buf_keys_a, *q);
    sycl::free(buf_keys_b, *q);
    sycl::free(buf_idx_a, *q);
    sycl::free(buf_idx_b, *q);

    return PGACCEL_OK;
  } catch (const sycl::exception& e) {
    fprintf(stderr, "pgaccel: SYCL radix sort failed: %s — falling back\n", e.what());
    return PGACCEL_UNSUPPORTED;
  } catch (const std::exception& e) {
    fprintf(stderr, "pgaccel: radix sort failed: %s — falling back\n", e.what());
    return PGACCEL_UNSUPPORTED;
  }
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

  try {
    auto alloc_u64 = [&](size_t n) { return sycl::malloc_device<uint64_t>(n, *q); };
    auto alloc_u32 = [&](size_t n) { return sycl::malloc_device<uint32_t>(n, *q); };

    uint64_t* buf_keys_a = alloc_u64(padded);
    uint64_t* buf_keys_b = alloc_u64(padded);
    uint32_t* buf_idx_a = alloc_u32(padded);
    uint32_t* buf_idx_b = alloc_u32(padded);
    uint32_t* d_group_hist = sycl::malloc_shared<uint32_t>(ngroups * RADIX_BINS, *q);

    if (!buf_keys_a || !buf_keys_b || !buf_idx_a || !buf_idx_b || !d_group_hist) {
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

    std::vector<uint32_t> scan_buf(ngroups * RADIX_BINS);

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

      uint32_t bin_total[RADIX_BINS];
      for (size_t b = 0; b < RADIX_BINS; ++b)
        bin_total[b] = 0;
      for (size_t g = 0; g < ngroups; ++g) {
        for (size_t b = 0; b < RADIX_BINS; ++b) {
          bin_total[b] += d_group_hist[g * RADIX_BINS + b];
        }
      }
      uint32_t bin_base[RADIX_BINS];
      bin_base[0] = 0;
      for (size_t b = 1; b < RADIX_BINS; ++b) {
        bin_base[b] = bin_base[b - 1] + bin_total[b - 1];
      }
      uint32_t running[RADIX_BINS];
      for (size_t b = 0; b < RADIX_BINS; ++b)
        running[b] = bin_base[b];
      for (size_t g = 0; g < ngroups; ++g) {
        for (size_t b = 0; b < RADIX_BINS; ++b) {
          scan_buf[g * RADIX_BINS + b] = running[b];
          running[b] += d_group_hist[g * RADIX_BINS + b];
        }
      }
      q->memcpy(d_group_hist, scan_buf.data(), ngroups * RADIX_BINS * sizeof(uint32_t))
          .wait_and_throw();

      {
        auto nd = sycl::nd_range<1>(sycl::range<1>(ngroups * RADIX_GROUP_SIZE),
                                    sycl::range<1>(RADIX_GROUP_SIZE));
        uint32_t* group_base_ptr = d_group_hist;
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
               const uint32_t base = group_base_ptr[gid * RADIX_BINS + my_digit];
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

    sycl::free(d_group_hist, *q);
    sycl::free(buf_keys_a, *q);
    sycl::free(buf_keys_b, *q);
    sycl::free(buf_idx_a, *q);
    sycl::free(buf_idx_b, *q);

    return PGACCEL_OK;
  } catch (const sycl::exception& e) {
    fprintf(stderr, "pgaccel: SYCL radix sort u64 failed: %s — falling back\n", e.what());
    return PGACCEL_UNSUPPORTED;
  } catch (const std::exception& e) {
    fprintf(stderr, "pgaccel: radix sort u64 failed: %s — falling back\n", e.what());
    return PGACCEL_UNSUPPORTED;
  }
}

/// GPU radix sort for int64 keys + indices (key-value).
static pgaccel_status sycl_radix_sort_kv_i64(int64_t* keys, uint32_t* indices, size_t count) {
  std::vector<uint64_t> ukeys(count);
  for (size_t i = 0; i < count; i++) {
    ukeys[i] = i64_to_sortable(keys[i]);
  }
  pgaccel_status st = sycl_radix_sort_kv_u64(ukeys.data(), indices, count);
  if (st != PGACCEL_OK)
    return st;
  for (size_t i = 0; i < count; i++) {
    keys[i] = sortable_to_i64(ukeys[i]);
  }
  return PGACCEL_OK;
}

/// GPU radix sort for int64 (values only).
static pgaccel_status sycl_radix_sort_i64(int64_t* data, size_t count) {
  std::vector<uint32_t> indices(count);
  for (size_t i = 0; i < count; i++)
    indices[i] = static_cast<uint32_t>(i);
  return sycl_radix_sort_kv_i64(data, indices.data(), count);
}

/// GPU radix sort for uint64 (values only).
[[maybe_unused]] static pgaccel_status sycl_radix_sort_u64(uint64_t* data, size_t count) {
  std::vector<uint32_t> indices(count);
  for (size_t i = 0; i < count; i++)
    indices[i] = static_cast<uint32_t>(i);
  return sycl_radix_sort_kv_u64(data, indices.data(), count);
}

/// GPU radix sort for int32 keys + indices (key-value).
static pgaccel_status sycl_radix_sort_kv_i32(int32_t* keys, uint32_t* indices, size_t count) {
  // Convert to sortable uint32, radix sort, convert back.
  std::vector<uint32_t> ukeys(count);
  for (size_t i = 0; i < count; i++) {
    ukeys[i] = i32_to_sortable(keys[i]);
  }

  pgaccel_status st = sycl_radix_sort_kv_u32(ukeys.data(), indices, count);
  if (st != PGACCEL_OK)
    return st;

  for (size_t i = 0; i < count; i++) {
    keys[i] = sortable_to_i32(ukeys[i]);
  }
  return PGACCEL_OK;
}

/// GPU radix sort for float32 keys + indices (key-value).
static pgaccel_status sycl_radix_sort_kv_f32(float* keys, uint32_t* indices, size_t count) {
  if (count > std::numeric_limits<uint32_t>::max())
    return PGACCEL_ERROR;

  std::vector<uint32_t> ukeys(count);
  std::vector<uint32_t> order(count);
  for (size_t i = 0; i < count; i++) {
    ukeys[i] = f32_to_sortable(keys[i]);
    order[i] = static_cast<uint32_t>(i);
  }

  pgaccel_status st = sycl_radix_sort_kv_u32(ukeys.data(), order.data(), count);
  if (st != PGACCEL_OK)
    return st;

  std::vector<float> original_keys(keys, keys + count);
  std::vector<uint32_t> original_indices(indices, indices + count);
  for (size_t i = 0; i < count; i++) {
    const uint32_t src = order[i];
    keys[i] = original_keys[src];
    indices[i] = original_indices[src];
  }

  return PGACCEL_OK;
}

/// GPU radix sort for double keys + indices (key-value).
static pgaccel_status sycl_radix_sort_kv_f64(double* keys, uint32_t* indices, size_t count) {
  if (count > std::numeric_limits<uint32_t>::max())
    return PGACCEL_ERROR;

  std::vector<uint64_t> ukeys(count);
  std::vector<uint32_t> order(count);
  for (size_t i = 0; i < count; i++) {
    ukeys[i] = f64_to_sortable(keys[i]);
    order[i] = static_cast<uint32_t>(i);
  }

  pgaccel_status st = sycl_radix_sort_kv_u64(ukeys.data(), order.data(), count);
  if (st != PGACCEL_OK)
    return st;

  std::vector<double> original_keys(keys, keys + count);
  std::vector<uint32_t> original_indices(indices, indices + count);
  for (size_t i = 0; i < count; i++) {
    const uint32_t src = order[i];
    keys[i] = original_keys[src];
    indices[i] = original_indices[src];
  }

  return PGACCEL_OK;
}

/// GPU radix sort for plain int32 (values only, no indices).
static pgaccel_status sycl_radix_sort_i32(int32_t* data, size_t count) {
  std::vector<uint32_t> indices(count);
  for (size_t i = 0; i < count; i++)
    indices[i] = static_cast<uint32_t>(i);
  return sycl_radix_sort_kv_i32(data, indices.data(), count);
}

/// GPU radix sort for plain float32 (values only, no indices).
static pgaccel_status sycl_radix_sort_f32(float* data, size_t count) {
  if (count > std::numeric_limits<uint32_t>::max())
    return PGACCEL_ERROR;

  std::vector<uint32_t> indices(count);
  for (size_t i = 0; i < count; i++)
    indices[i] = static_cast<uint32_t>(i);
  return sycl_radix_sort_kv_f32(data, indices.data(), count);
}

/// GPU radix sort for plain double (values only, no indices).
static pgaccel_status sycl_radix_sort_f64(double* data, size_t count) {
  if (count > std::numeric_limits<uint32_t>::max())
    return PGACCEL_ERROR;

  std::vector<uint32_t> indices(count);
  for (size_t i = 0; i < count; i++)
    indices[i] = static_cast<uint32_t>(i);
  return sycl_radix_sort_kv_f64(data, indices.data(), count);
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
  // and a work-group-per-tile scatter with host-computed per-group
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
        // uint32 radix: no conversion needed, just indices-then-drop.
        std::vector<uint32_t> indices(count);
        for (size_t i = 0; i < count; ++i)
          indices[i] = static_cast<uint32_t>(i);
        st = sycl_radix_sort_kv_u32(reinterpret_cast<uint32_t*>(data), indices.data(), count);
      } else if constexpr (std::is_same_v<T, int64_t>) {
        st = sycl_radix_sort_i64(data, count);
      } else if constexpr (std::is_same_v<T, uint64_t>) {
        std::vector<uint32_t> indices(count);
        for (size_t i = 0; i < count; ++i)
          indices[i] = static_cast<uint32_t>(i);
        st = sycl_radix_sort_kv_u64(reinterpret_cast<uint64_t*>(data), indices.data(), count);
      }
      if (st == PGACCEL_OK) {
        pgaccel_record_gpu_exec();
        return st;
      }
      // Fall through to bitonic on radix failure.
    }
  }

  // Bitonic sort uses compare-and-swap only — reliable on Metal.
  {
    pgaccel_status st = sycl_bitonic_sort(data, count);
    if (st == PGACCEL_OK) {
      pgaccel_record_gpu_exec();
      return st;
    }
  }
  return PGACCEL_ERROR_NO_DEVICE;
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
    }
  }

  // Bitonic sort uses compare-and-swap only — reliable on Metal.
  {
    pgaccel_status st = sycl_bitonic_sort_kv(keys, indices, count);
    if (st == PGACCEL_OK) {
      pgaccel_record_gpu_exec();
      return st;
    }
  }
  return PGACCEL_ERROR_NO_DEVICE;
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

// ===========================================================================
// Public C API
// ===========================================================================

extern "C" {

pgaccel_status pgaccel_sort_f32(float* data, size_t count) {
  return dispatch_sort(data, count);
}

pgaccel_status pgaccel_sort_f64(double* data, size_t count) {
  return dispatch_sort_fp_checked(data, count);
}

pgaccel_status pgaccel_sort_i32(int32_t* data, size_t count) {
  return dispatch_sort(data, count);
}

pgaccel_status pgaccel_sort_i64(int64_t* data, size_t count) {
  return dispatch_sort(data, count);
}

pgaccel_status pgaccel_sort_kv_f32(float* keys, uint32_t* indices, size_t count) {
  return dispatch_sort_kv(keys, indices, count);
}

pgaccel_status pgaccel_sort_kv_f64(double* keys, uint32_t* indices, size_t count) {
  return dispatch_sort_kv_fp_checked(keys, indices, count);
}

pgaccel_status pgaccel_sort_kv_i32(int32_t* keys, uint32_t* indices, size_t count) {
  return dispatch_sort_kv(keys, indices, count);
}

pgaccel_status pgaccel_sort_kv_i64(int64_t* keys, uint32_t* indices, size_t count) {
  return dispatch_sort_kv(keys, indices, count);
}

}  // extern "C"

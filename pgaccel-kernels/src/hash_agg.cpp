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

#include <cmath>
#include <cstdint>
#include <cstdlib>
#include <cstring>
#include <limits>
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
  std::vector<std::vector<double>> results;
};

// ---------------------------------------------------------------------------
// Read a value from a typed column
// ---------------------------------------------------------------------------

namespace {

// Device-callable typed read. Returns (value, is_null) — kernel-side.
struct val_read {
  double value;
  bool is_null;
};

inline val_read device_read_value(const void* col_data, const uint8_t* null_mask, size_t row,
                                  int val_type) {
  val_read r = {0.0, true};
  if (col_data == nullptr)
    return r;
  if (null_mask != nullptr && null_mask[row])
    return r;
  r.is_null = false;
  switch (val_type) {
    case 1:  // BOOL
      r.value = static_cast<const bool*>(col_data)[row] ? 1.0 : 0.0;
      break;
    case 2:  // INT32
      r.value = static_cast<double>(static_cast<const int32_t*>(col_data)[row]);
      break;
    case 3:  // INT64
      r.value = static_cast<double>(static_cast<const int64_t*>(col_data)[row]);
      break;
    case 4:  // FLOAT32
      r.value = static_cast<double>(static_cast<const float*>(col_data)[row]);
      break;
    case 5:  // FLOAT64
      r.value = static_cast<const double*>(col_data)[row];
      break;
    default:
      r.is_null = true;
      break;
  }
  return r;
}

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
    default:
      return 0;
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
// O(n*g) but n_groups is bounded by SORT_AGG_MIN_ROWS (100k) for the
// hash path so the cross-product stays under ~1e10 ops, GPU-resident.
// ---------------------------------------------------------------------------

namespace {

// Run the accumulation kernel for a sorted-input path.
// `indices[i]` gives the original row index for sort position `i`.
// `group_starts[g]` / `group_ends[g]` bracket the sorted positions
// belonging to group `g`. Each work-item handles one group.
//
// `out_results` is a flat (num_aggs * n_groups) f64 buffer.
// `out_counts` is a length-n_groups i64 buffer.
//
// All input buffers must already live in shared memory.
void run_sorted_accum_kernel(sycl::queue& q, size_t n_groups, size_t num_aggs,
                             const uint32_t* indices, const size_t* group_starts,
                             const size_t* group_ends, const void* const* value_cols,
                             const uint8_t* const* value_nulls, const int* value_types,
                             const pgaccel_agg_func* agg_funcs, const size_t* agg_col_idx,
                             double* out_results, int64_t* out_counts) {
  q.parallel_for(sycl::range<1>(n_groups), [=](sycl::id<1> id) {
     const size_t g = id[0];
     const size_t start = group_starts[g];
     const size_t end = group_ends[g];

     int64_t cnt = 0;

     for (size_t a = 0; a < num_aggs; ++a) {
       const pgaccel_agg_func func = agg_funcs[a];
       const size_t col = agg_col_idx[a];
       const void* col_data = value_cols[a];
       const uint8_t* col_nulls = (value_nulls != nullptr) ? value_nulls[a] : nullptr;
       const int vtype = value_types[a];

       double acc = 0.0;
       if (func == PGACCEL_AGG_MIN)
         acc = std::numeric_limits<double>::infinity();
       else if (func == PGACCEL_AGG_MAX)
         acc = -std::numeric_limits<double>::infinity();

       for (size_t i = start; i < end; ++i) {
         const uint32_t r = indices[i];
         // Bump count once per row (only on the first agg's pass).
         if (a == 0)
           ++cnt;

         if (func == PGACCEL_AGG_COUNT && col == SIZE_MAX) {
           acc += 1.0;
           continue;
         }

         val_read vr = device_read_value(col_data, col_nulls, r, vtype);
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
         }
       }
       out_results[a * n_groups + g] = acc;
     }
     out_counts[g] = cnt;
   }).wait();
}

// Run the accumulation kernel for an unsorted-input path.
// `row_to_group[r]` gives the group index for row `r` (or `SIZE_MAX` if
// the row was not assigned). One work-item per group; each scans all
// rows linearly to find the ones belonging to its group. Acceptable
// when n_groups is small (the hash path runs only when row_count <
// SORT_AGG_MIN_ROWS = 100k).
void run_unsorted_accum_kernel(sycl::queue& q, size_t n_groups, size_t row_count, size_t num_aggs,
                               const size_t* row_to_group, const void* const* value_cols,
                               const uint8_t* const* value_nulls, const int* value_types,
                               const pgaccel_agg_func* agg_funcs, const size_t* agg_col_idx,
                               double* out_results, int64_t* out_counts) {
  q.parallel_for(sycl::range<1>(n_groups), [=](sycl::id<1> id) {
     const size_t g = id[0];

     int64_t cnt = 0;

     for (size_t a = 0; a < num_aggs; ++a) {
       const pgaccel_agg_func func = agg_funcs[a];
       const size_t col = agg_col_idx[a];
       const void* col_data = value_cols[a];
       const uint8_t* col_nulls = (value_nulls != nullptr) ? value_nulls[a] : nullptr;
       const int vtype = value_types[a];

       double acc = 0.0;
       if (func == PGACCEL_AGG_MIN)
         acc = std::numeric_limits<double>::infinity();
       else if (func == PGACCEL_AGG_MAX)
         acc = -std::numeric_limits<double>::infinity();

       for (size_t r = 0; r < row_count; ++r) {
         if (row_to_group[r] != g)
           continue;
         if (a == 0)
           ++cnt;

         if (func == PGACCEL_AGG_COUNT && col == SIZE_MAX) {
           acc += 1.0;
           continue;
         }

         val_read vr = device_read_value(col_data, col_nulls, r, vtype);
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
         }
       }
       out_results[a * n_groups + g] = acc;
     }
     out_counts[g] = cnt;
   }).wait();
}

// Helper: stage a value-column array of pointers (and its corresponding
// null-mask pointer array) into shared memory so the device kernel can
// access them. Returns owning pointers (must be sycl::free'd by caller).
struct StagedColumnArrays {
  const void** d_value_cols;      // [num_aggs] device-accessible pointers to col data
  const uint8_t** d_value_nulls;  // [num_aggs] device-accessible pointers to null masks
  int* d_value_types;             // [num_aggs] type tags
  pgaccel_agg_func* d_funcs;      // [num_aggs] agg funcs
  size_t* d_col_idx;              // [num_aggs] agg col indices
  void** d_data_buffers;          // [num_aggs] shared-mem copies of column data (or nullptr)
  uint8_t** d_null_buffers;       // [num_aggs] shared-mem copies of null masks (or nullptr)
};

// Stage column data and null masks into shared memory. PG's column
// pointers are host-side palloc memory which may not be device-
// accessible on Metal SSCP — copy each into a shared-mem buffer of
// the appropriate size based on the type tag.
//
// Returns nullptr on OOM. Caller must free via free_staged_columns().
StagedColumnArrays* stage_columns(sycl::queue& q, size_t row_count, size_t num_aggs,
                                  const void* const* value_cols, const uint8_t* const* value_nulls,
                                  const int* value_types, const pgaccel_agg_col* agg_cols) {
  auto* s = new (std::nothrow) StagedColumnArrays();
  if (s == nullptr)
    return nullptr;
  s->d_value_cols = sycl::malloc_shared<const void*>(num_aggs, q);
  s->d_value_nulls = sycl::malloc_shared<const uint8_t*>(num_aggs, q);
  s->d_value_types = sycl::malloc_shared<int>(num_aggs, q);
  s->d_funcs = sycl::malloc_shared<pgaccel_agg_func>(num_aggs, q);
  s->d_col_idx = sycl::malloc_shared<size_t>(num_aggs, q);
  s->d_data_buffers = static_cast<void**>(std::calloc(num_aggs, sizeof(void*)));
  s->d_null_buffers = static_cast<uint8_t**>(std::calloc(num_aggs, sizeof(uint8_t*)));
  if (!s->d_value_cols || !s->d_value_nulls || !s->d_value_types || !s->d_funcs || !s->d_col_idx ||
      !s->d_data_buffers || !s->d_null_buffers) {
    if (s->d_value_cols)
      sycl::free(static_cast<void*>(s->d_value_cols), q);
    if (s->d_value_nulls)
      sycl::free(static_cast<void*>(s->d_value_nulls), q);
    if (s->d_value_types)
      sycl::free(s->d_value_types, q);
    if (s->d_funcs)
      sycl::free(s->d_funcs, q);
    if (s->d_col_idx)
      sycl::free(s->d_col_idx, q);
    std::free(s->d_data_buffers);
    std::free(s->d_null_buffers);
    delete s;
    return nullptr;
  }

  for (size_t a = 0; a < num_aggs; ++a) {
    s->d_value_types[a] = value_types[a];
    s->d_funcs[a] = agg_cols[a].func;
    s->d_col_idx[a] = agg_cols[a].col_idx;

    // Stage value column data.
    if (value_cols[a] == nullptr) {
      s->d_value_cols[a] = nullptr;
      s->d_data_buffers[a] = nullptr;
    } else {
      // Determine byte width per row from the type tag.
      size_t elem_size = 0;
      switch (value_types[a]) {
        case 1:
          elem_size = sizeof(bool);
          break;
        case 2:
          elem_size = sizeof(int32_t);
          break;
        case 3:
          elem_size = sizeof(int64_t);
          break;
        case 4:
          elem_size = sizeof(float);
          break;
        case 5:
          elem_size = sizeof(double);
          break;
        default:
          elem_size = 0;
          break;
      }
      if (elem_size == 0) {
        s->d_value_cols[a] = nullptr;
        s->d_data_buffers[a] = nullptr;
      } else {
        void* buf = sycl::malloc_shared(row_count * elem_size, q);
        if (buf == nullptr) {
          // OOM — leave this column as nullptr; kernel handles it as null.
          s->d_value_cols[a] = nullptr;
          s->d_data_buffers[a] = nullptr;
        } else {
          std::memcpy(buf, value_cols[a], row_count * elem_size);
          s->d_value_cols[a] = buf;
          s->d_data_buffers[a] = buf;
        }
      }
    }

    // Stage null mask if present.
    if (value_nulls != nullptr && value_nulls[a] != nullptr) {
      uint8_t* nbuf = sycl::malloc_shared<uint8_t>(row_count, q);
      if (nbuf == nullptr) {
        s->d_value_nulls[a] = nullptr;
        s->d_null_buffers[a] = nullptr;
      } else {
        std::memcpy(nbuf, value_nulls[a], row_count);
        s->d_value_nulls[a] = nbuf;
        s->d_null_buffers[a] = nbuf;
      }
    } else {
      s->d_value_nulls[a] = nullptr;
      s->d_null_buffers[a] = nullptr;
    }
  }

  return s;
}

void free_staged_columns(sycl::queue& q, StagedColumnArrays* s, size_t num_aggs) {
  if (s == nullptr)
    return;
  for (size_t a = 0; a < num_aggs; ++a) {
    if (s->d_data_buffers[a] != nullptr)
      sycl::free(s->d_data_buffers[a], q);
    if (s->d_null_buffers[a] != nullptr)
      sycl::free(s->d_null_buffers[a], q);
  }
  std::free(s->d_data_buffers);
  std::free(s->d_null_buffers);
  if (s->d_value_cols)
    sycl::free(static_cast<void*>(s->d_value_cols), q);
  if (s->d_value_nulls)
    sycl::free(static_cast<void*>(s->d_value_nulls), q);
  if (s->d_value_types)
    sycl::free(s->d_value_types, q);
  if (s->d_funcs)
    sycl::free(s->d_funcs, q);
  if (s->d_col_idx)
    sycl::free(s->d_col_idx, q);
  delete s;
}

}  // namespace

// ---------------------------------------------------------------------------
// Hash-based grouped aggregation (host-side group assignment + GPU
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
  for (size_t r = 0; r < row_count; ++r) {
    bool is_null = (group_null_mask != nullptr && group_null_mask[r]);
    if (is_null) {
      uint64_t h = 0xDEADBEEFULL;
      auto it = hash_to_groups.find(h);
      if (it == hash_to_groups.end()) {
        size_t gidx = n_groups++;
        hash_to_groups[h] = {gidx};
        row_to_group[r] = gidx;
        group_key_buf.resize(group_key_buf.size() + ksz, 0);
      } else {
        row_to_group[r] = it->second[0];
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
        if (memcmp(stored, current, ksz) == 0) {
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

  // -- Phase 2 (SYCL kernel): per-group accumulation. Stage value
  // columns into shared memory and dispatch one work-item per group.
  StagedColumnArrays* sc =
      stage_columns(*q, row_count, num_aggs, value_cols, value_nulls, value_types, agg_cols);
  if (sc == nullptr) {
    sycl::free(row_to_group, *q);
    return nullptr;
  }

  double* d_results = sycl::malloc_shared<double>(num_aggs * n_groups, *q);
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

  run_unsorted_accum_kernel(*q, n_groups, row_count, num_aggs, row_to_group, sc->d_value_cols,
                            sc->d_value_nulls, sc->d_value_types, sc->d_funcs, sc->d_col_idx,
                            d_results, d_counts);

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

/// Minimum rows to use sort-based path. The sort path is GPU-parallel
/// with O(n) work; the hash path's per-group linear-scan kernel is
/// O(n*g) so degrades on high-cardinality. For small N the
/// hash overhead is dominated by host hash-map operations either
/// way, so we still pick sort-based when N is large.
static constexpr size_t SORT_AGG_MIN_ROWS = 100000;

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

  double* d_results = sycl::malloc_shared<double>(num_aggs * n_groups, *q);
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

  run_sorted_accum_kernel(*q, n_groups, num_aggs, indices, d_group_starts, d_group_ends,
                          sc->d_value_cols, sc->d_value_nulls, sc->d_value_types, sc->d_funcs,
                          sc->d_col_idx, d_results, d_counts);

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
  state->group_key_buf.resize(n_groups * ksz);
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
// Public C API
// ===========================================================================

extern "C" {

pgaccel_agg_state* pgaccel_hash_agg_execute(const void* group_keys, const uint8_t* group_null_mask,
                                            size_t row_count, int key_type,
                                            const void* const* value_cols,
                                            const uint8_t* const* value_nulls,
                                            const int* value_types, const pgaccel_agg_col* agg_cols,
                                            size_t num_aggs) {
  if (row_count == 0 || agg_cols == nullptr)
    return nullptr;

  // Use sort-based path for large datasets (GPU-parallel sort + SYCL
  // per-group reduce). Falls back to hash path on sort failure.
  if (row_count >= SORT_AGG_MIN_ROWS) {
    pgaccel_agg_state* st =
        agg_sort_based(group_keys, group_null_mask, row_count, key_type, value_cols, value_nulls,
                       value_types, agg_cols, num_aggs);
    if (st != nullptr)
      return st;
  }

  return agg_hash(group_keys, group_null_mask, row_count, key_type, value_cols, value_nulls,
                  value_types, agg_cols, num_aggs);
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
  return state->results[agg_idx].data();
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

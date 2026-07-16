/*
 * hash_join.cpp -- GPU hash join build/probe kernels.
 *
 * This is intentionally a narrow selected-path implementation:
 *   - INNER equi-join keys represented as INT32 or INT64 only;
 *   - NULL keys are skipped on both build and probe sides;
 *   - duplicate build keys are chained in device memory;
 *   - probe output is caller-bounded and reports overflow instead of
 *     truncating.
 *
 * Unsupported key types fail closed so the planner can keep declining those
 * shapes honestly.
 */

#include <sycl/sycl.hpp>

#include <cstdint>
#include <cstdio>
#include <cstdlib>
#include <cstring>
#include <exception>
#include <limits>
#include <new>

#include "pgaccel_hash_join.h"
#include "pgaccel_queue.h"

struct pgaccel_hash_table {
  pgaccel_key_type key_type;
  size_t count;
  size_t capacity;
  sycl::queue* queue;
  void* d_keys;
  uint8_t* d_null_mask;
  uint32_t* d_indices;
  int32_t* d_heads;
  int32_t* d_next;
  bool owns_input_buffers;
};

namespace {

static constexpr int32_t EMPTY_HEAD = -1;

static inline uint64_t hash64(uint64_t k) {
  k ^= k >> 33;
  k *= 0xff51afd7ed558ccdULL;
  k ^= k >> 33;
  k *= 0xc4ceb9fe1a85ec53ULL;
  k ^= k >> 33;
  return k;
}

template <typename K>
static inline uint64_t hash_key(K key) {
  if constexpr (sizeof(K) == sizeof(uint32_t)) {
    return hash64(static_cast<uint64_t>(static_cast<uint32_t>(key)));
  } else {
    return hash64(static_cast<uint64_t>(key));
  }
}

static bool next_power_of_two_checked(size_t n, size_t* out) {
  if (out == nullptr)
    return false;
  size_t p = 1;
  while (p < n) {
    if (p > (std::numeric_limits<size_t>::max() / 2)) {
      return false;
    }
    p *= 2;
  }
  *out = p;
  return true;
}

static bool hash_join_capacity(size_t non_null_rows, size_t* out_capacity) {
  if (out_capacity == nullptr)
    return false;
  if (non_null_rows > (std::numeric_limits<size_t>::max() / 2)) {
    return false;
  }
  size_t requested = non_null_rows * 2;
  if (requested < 2)
    requested = 2;

  size_t rounded = 0;
  if (!next_power_of_two_checked(requested, &rounded)) {
    return false;
  }
  if (rounded < 16)
    rounded = 16;
  if (rounded > static_cast<size_t>(std::numeric_limits<int32_t>::max())) {
    return false;
  }
  *out_capacity = rounded;
  return true;
}

static size_t key_size(pgaccel_key_type key_type) {
  switch (key_type) {
    case PGACCEL_KEY_INT32:
      return sizeof(int32_t);
    case PGACCEL_KEY_INT64:
      return sizeof(int64_t);
    default:
      return 0;
  }
}

static sycl::queue* get_queue() {
  return pgaccel_get_queue();
}

static bool is_metal_backend() {
  const pgaccel_platform_caps caps = pgaccel_get_caps();
  return std::strcmp(caps.backend_name, "metal") == 0;
}

static bool has_count_storage(const pgaccel_hash_table* ht) {
  return ht != nullptr && ht->d_keys != nullptr && ht->d_heads != nullptr &&
         ht->d_next != nullptr && ht->capacity != 0 && (ht->capacity & (ht->capacity - 1)) == 0;
}

static bool has_probe_storage(const pgaccel_hash_table* ht) {
  return has_count_storage(ht) && ht->d_indices != nullptr;
}

static void free_table_storage(pgaccel_hash_table* ht) {
  if (ht == nullptr || ht->queue == nullptr)
    return;
  sycl::queue& q = *ht->queue;
  if (ht->owns_input_buffers && ht->d_keys != nullptr)
    sycl::free(ht->d_keys, q);
  if (ht->owns_input_buffers && ht->d_null_mask != nullptr)
    sycl::free(ht->d_null_mask, q);
  if (ht->owns_input_buffers && ht->d_indices != nullptr)
    sycl::free(ht->d_indices, q);
  if (ht->d_heads != nullptr)
    sycl::free(ht->d_heads, q);
  if (ht->d_next != nullptr)
    sycl::free(ht->d_next, q);
  ht->d_keys = nullptr;
  ht->d_null_mask = nullptr;
  ht->d_indices = nullptr;
  ht->d_heads = nullptr;
  ht->d_next = nullptr;
  ht->owns_input_buffers = false;
}

template <typename K>
static bool build_hash_table_serial_kernel(sycl::queue& q, const K* d_keys,
                                           const uint8_t* d_nulls, int32_t* d_heads,
                                           int32_t* d_next, size_t count,
                                           size_t table_capacity) {
  uint32_t* d_build_failed = sycl::malloc_shared<uint32_t>(1, q);
  if (d_build_failed == nullptr) {
    return false;
  }
  *d_build_failed = 0;
  const size_t mask = table_capacity - 1;

  try {
    q.single_task([=]() {
       for (size_t row = 0; row < count; ++row) {
         if (d_nulls != nullptr && d_nulls[row] != 0)
           continue;

         const K key = d_keys[row];
         const uint64_t h = hash_key<K>(key);
         const int32_t row_i = static_cast<int32_t>(row);
         bool inserted = false;

         for (size_t attempt = 0; attempt < table_capacity; ++attempt) {
           const size_t slot = (h + attempt) & mask;
           const int32_t head = d_heads[slot];
           if (head == EMPTY_HEAD) {
             d_heads[slot] = row_i;
             d_next[row] = EMPTY_HEAD;
             inserted = true;
             break;
           }
           if (d_keys[static_cast<size_t>(head)] != key) {
             continue;
           }
           d_next[row] = head;
           d_heads[slot] = row_i;
           inserted = true;
           break;
         }

         if (!inserted) {
           *d_build_failed = 1;
           return;
         }
       }
     }).wait_and_throw();
    const bool built = *d_build_failed == 0;
    sycl::free(d_build_failed, q);
    return built;
  } catch (...) {
    sycl::free(d_build_failed, q);
    throw;
  }
}

template <typename K>
static void build_hash_table_kernel(sycl::queue& q, const K* d_keys, const uint8_t* d_nulls,
                                    int32_t* d_heads, int32_t* d_next, size_t count,
                                    size_t table_capacity) {
  const size_t mask = table_capacity - 1;

  q.parallel_for(sycl::range<1>(count), [=](sycl::id<1> id) {
     const size_t row = id[0];
     if (d_nulls != nullptr && d_nulls[row] != 0)
       return;

     const K key = d_keys[row];
     const uint64_t h = hash_key<K>(key);
     const int32_t row_i = static_cast<int32_t>(row);

     for (size_t attempt = 0; attempt < table_capacity; ++attempt) {
       const size_t slot = (h + attempt) & mask;
       sycl::atomic_ref<int32_t, sycl::memory_order::acq_rel, sycl::memory_scope::device,
                        sycl::access::address_space::global_space>
           head_ref(d_heads[slot]);

       int32_t expected = EMPTY_HEAD;
       if (head_ref.compare_exchange_strong(expected, row_i)) {
         d_next[row] = EMPTY_HEAD;
         return;
       }

       int32_t head = expected;
       if (d_keys[static_cast<size_t>(head)] != key) {
         continue;
       }

       for (;;) {
         d_next[row] = head;
         int32_t compare = head;
         if (head_ref.compare_exchange_strong(compare, row_i)) {
           return;
         }
         head = compare;
       }
     }
   }).wait_and_throw();
}

template <typename K>
static pgaccel_hash_table* build_typed(const K* keys, const uint8_t* null_mask,
                                       const uint32_t* indices, size_t count,
                                       pgaccel_key_type key_type) {
  if (keys == nullptr || indices == nullptr || count == 0 ||
      count > static_cast<size_t>(std::numeric_limits<int32_t>::max())) {
    return nullptr;
  }

  sycl::queue* q = get_queue();
  if (q == nullptr) {
    return nullptr;
  }

  size_t capacity = 0;
  if (!hash_join_capacity(count, &capacity)) {
    return nullptr;
  }

  K* d_keys = nullptr;
  uint8_t* d_nulls = nullptr;
  uint32_t* d_indices = nullptr;
  int32_t* d_heads = nullptr;
  int32_t* d_next = nullptr;

  auto cleanup = [&]() {
    if (d_keys != nullptr)
      sycl::free(d_keys, *q);
    if (d_nulls != nullptr)
      sycl::free(d_nulls, *q);
    if (d_indices != nullptr)
      sycl::free(d_indices, *q);
    if (d_heads != nullptr)
      sycl::free(d_heads, *q);
    if (d_next != nullptr)
      sycl::free(d_next, *q);
  };

  try {
    d_keys = sycl::malloc_device<K>(count, *q);
    d_indices = sycl::malloc_device<uint32_t>(count, *q);
    d_next = sycl::malloc_device<int32_t>(count, *q);
    d_heads = sycl::malloc_device<int32_t>(capacity, *q);
    if (null_mask != nullptr)
      d_nulls = sycl::malloc_device<uint8_t>(count, *q);
    if (d_keys == nullptr || d_indices == nullptr || d_next == nullptr || d_heads == nullptr ||
        (null_mask != nullptr && d_nulls == nullptr)) {
      cleanup();
      return nullptr;
    }

    q->memcpy(d_keys, keys, count * sizeof(K));
    q->memcpy(d_indices, indices, count * sizeof(uint32_t));
    q->fill(d_next, EMPTY_HEAD, count);
    q->fill(d_heads, EMPTY_HEAD, capacity);
    if (d_nulls != nullptr)
      q->memcpy(d_nulls, null_mask, count * sizeof(uint8_t));
    q->wait_and_throw();

    bool built = false;
    if (is_metal_backend()) {
      built = build_hash_table_serial_kernel<K>(*q, d_keys, d_nulls, d_heads, d_next, count,
                                                capacity);
    } else {
      build_hash_table_kernel<K>(*q, d_keys, d_nulls, d_heads, d_next, count, capacity);
      built = true;
    }
    if (!built) {
      cleanup();
      return nullptr;
    }

    auto* ht = new (std::nothrow) pgaccel_hash_table{};
    if (ht == nullptr) {
      cleanup();
      return nullptr;
    }
    ht->key_type = key_type;
    ht->count = count;
    ht->capacity = capacity;
    ht->queue = q;
    ht->d_keys = d_keys;
    ht->d_null_mask = d_nulls;
    ht->d_indices = d_indices;
    ht->d_heads = d_heads;
    ht->d_next = d_next;
    ht->owns_input_buffers = true;
    pgaccel_record_gpu_exec();

    return ht;
  } catch (const sycl::exception& e) {
    std::fprintf(stderr, "pgaccel: hash_join build kernel failed: %s\n", e.what());
    cleanup();
    return nullptr;
  } catch (const std::exception& e) {
    std::fprintf(stderr, "pgaccel: hash_join build failed: %s\n", e.what());
    cleanup();
    return nullptr;
  }
}

template <typename K>
static pgaccel_hash_table* build_device_count_typed(const K* device_keys,
                                                    const uint8_t* device_null_mask, size_t count,
                                                    pgaccel_key_type key_type) {
  if (device_keys == nullptr || count == 0 ||
      count > static_cast<size_t>(std::numeric_limits<int32_t>::max())) {
    return nullptr;
  }

  sycl::queue* q = get_queue();
  if (q == nullptr) {
    return nullptr;
  }

  size_t capacity = 0;
  if (!hash_join_capacity(count, &capacity)) {
    return nullptr;
  }

  int32_t* d_heads = nullptr;
  int32_t* d_next = nullptr;

  auto cleanup = [&]() {
    if (d_heads != nullptr)
      sycl::free(d_heads, *q);
    if (d_next != nullptr)
      sycl::free(d_next, *q);
  };

  try {
    d_next = sycl::malloc_device<int32_t>(count, *q);
    d_heads = sycl::malloc_device<int32_t>(capacity, *q);
    if (d_next == nullptr || d_heads == nullptr) {
      cleanup();
      return nullptr;
    }

    q->fill(d_next, EMPTY_HEAD, count);
    q->fill(d_heads, EMPTY_HEAD, capacity);
    q->wait_and_throw();

    const K* d_keys = device_keys;
    const uint8_t* d_nulls = device_null_mask;
    bool built = false;
    if (is_metal_backend()) {
      built = build_hash_table_serial_kernel<K>(*q, d_keys, d_nulls, d_heads, d_next, count,
                                                capacity);
    } else {
      build_hash_table_kernel<K>(*q, d_keys, d_nulls, d_heads, d_next, count, capacity);
      built = true;
    }
    if (!built) {
      cleanup();
      return nullptr;
    }

    auto* ht = new (std::nothrow) pgaccel_hash_table{};
    if (ht == nullptr) {
      cleanup();
      return nullptr;
    }
    ht->key_type = key_type;
    ht->count = count;
    ht->capacity = capacity;
    ht->queue = q;
    ht->d_keys = const_cast<K*>(device_keys);
    ht->d_null_mask = const_cast<uint8_t*>(device_null_mask);
    ht->d_indices = nullptr;
    ht->d_heads = d_heads;
    ht->d_next = d_next;
    ht->owns_input_buffers = false;
    pgaccel_record_gpu_exec();
    return ht;
  } catch (const sycl::exception& e) {
    std::fprintf(stderr, "pgaccel: resident hash_join build kernel failed: %s\n", e.what());
    cleanup();
    return nullptr;
  } catch (const std::exception& e) {
    std::fprintf(stderr, "pgaccel: resident hash_join build failed: %s\n", e.what());
    cleanup();
    return nullptr;
  }
}

template <typename K>
static pgaccel_status probe_typed(const pgaccel_hash_table* ht, const K* outer_keys,
                                  const uint8_t* outer_null_mask, size_t outer_count,
                                  uint32_t* match_pairs, size_t max_matches, size_t* match_count) {
  if (ht == nullptr || outer_keys == nullptr || match_count == nullptr ||
      (max_matches > 0 && match_pairs == nullptr)) {
    return PGACCEL_ERROR;
  }
  std::memset(match_count, 0, sizeof(*match_count));
  if (outer_count == 0) {
    return PGACCEL_OK;
  }
  if (outer_count > static_cast<size_t>(std::numeric_limits<uint32_t>::max()) ||
      max_matches > static_cast<size_t>(std::numeric_limits<uint32_t>::max()) ||
      max_matches > (std::numeric_limits<size_t>::max() / 2)) {
    return PGACCEL_UNSUPPORTED;
  }
  if (!has_probe_storage(ht)) {
    return PGACCEL_UNSUPPORTED;
  }

  sycl::queue* q = get_queue();
  if (q == nullptr || ht->queue == nullptr || q != ht->queue) {
    return PGACCEL_ERROR_NO_DEVICE;
  }

  const size_t alloc_outer = outer_count;
  K* d_outer_keys = nullptr;
  uint8_t* d_outer_nulls = nullptr;
  uint32_t* d_pairs = nullptr;
  uint32_t* d_match_count = nullptr;
  uint32_t* d_overflow = nullptr;
  size_t* d_final_match_count = nullptr;
  pgaccel_status* d_status = nullptr;

  try {
    d_outer_keys = sycl::malloc_device<K>(alloc_outer, *q);
    if (outer_null_mask != nullptr) {
      d_outer_nulls = sycl::malloc_device<uint8_t>(alloc_outer, *q);
    }
    const size_t pair_u32s = max_matches * 2;
    if (pair_u32s > 0) {
      d_pairs = sycl::malloc_device<uint32_t>(pair_u32s, *q);
    }
    d_match_count = sycl::malloc_device<uint32_t>(1, *q);
    d_overflow = sycl::malloc_device<uint32_t>(1, *q);
    d_final_match_count = sycl::malloc_device<size_t>(1, *q);
    d_status = sycl::malloc_device<pgaccel_status>(1, *q);

    if (d_outer_keys == nullptr || (outer_null_mask != nullptr && d_outer_nulls == nullptr) ||
        (pair_u32s > 0 && d_pairs == nullptr) || d_match_count == nullptr ||
        d_overflow == nullptr || d_final_match_count == nullptr || d_status == nullptr) {
      if (d_outer_keys != nullptr)
        sycl::free(d_outer_keys, *q);
      if (d_outer_nulls != nullptr)
        sycl::free(d_outer_nulls, *q);
      if (d_pairs != nullptr)
        sycl::free(d_pairs, *q);
      if (d_match_count != nullptr)
        sycl::free(d_match_count, *q);
      if (d_overflow != nullptr)
        sycl::free(d_overflow, *q);
      if (d_final_match_count != nullptr)
        sycl::free(d_final_match_count, *q);
      if (d_status != nullptr)
        sycl::free(d_status, *q);
      return PGACCEL_OOM;
    }

    q->fill(d_match_count, 0u, 1);
    q->fill(d_overflow, 0u, 1);
    if (pair_u32s > 0)
      q->fill(d_pairs, 0u, pair_u32s);
    q->memcpy(d_outer_keys, outer_keys, outer_count * sizeof(K));
    if (outer_null_mask != nullptr) {
      q->memcpy(d_outer_nulls, outer_null_mask, outer_count * sizeof(uint8_t));
    }
    q->wait_and_throw();

    const K* d_build_keys = static_cast<const K*>(ht->d_keys);
    const int32_t* d_heads = ht->d_heads;
    const int32_t* d_next = ht->d_next;
    const uint32_t* d_indices = ht->d_indices;
    const size_t table_capacity = ht->capacity;
    const size_t mask = table_capacity - 1;
    const uint32_t max_matches_u32 = static_cast<uint32_t>(max_matches);

    q->parallel_for(sycl::range<1>(outer_count), [=](sycl::id<1> id) {
       const size_t outer_row = id[0];
       if (d_outer_nulls != nullptr && d_outer_nulls[outer_row] != 0) {
         return;
       }

       const K key = d_outer_keys[outer_row];
       const uint64_t h = hash_key<K>(key);

       for (size_t attempt = 0; attempt < table_capacity; ++attempt) {
         const size_t slot = (h + attempt) & mask;
         const int32_t head = d_heads[slot];
         if (head == EMPTY_HEAD) {
           return;
         }
         if (d_build_keys[static_cast<size_t>(head)] != key) {
           continue;
         }

         int32_t cur = head;
         while (cur != EMPTY_HEAD) {
           sycl::atomic_ref<uint32_t, sycl::memory_order::acq_rel, sycl::memory_scope::device,
                            sycl::access::address_space::global_space>
               count_ref(*d_match_count);
           const uint32_t pos = count_ref.fetch_add(1u);
           if (pos < max_matches_u32) {
             const size_t out = static_cast<size_t>(pos) * 2;
             d_pairs[out] = static_cast<uint32_t>(outer_row);
             d_pairs[out + 1] = d_indices[static_cast<size_t>(cur)];
           } else {
             sycl::atomic_ref<uint32_t, sycl::memory_order::relaxed, sycl::memory_scope::device,
                              sycl::access::address_space::global_space>
                 overflow_ref(*d_overflow);
             overflow_ref.store(1u);
           }
           cur = d_next[static_cast<size_t>(cur)];
         }
         return;
       }
     }).wait_and_throw();

    q->single_task([=]() {
       const uint32_t produced = d_match_count[0];
       d_final_match_count[0] = static_cast<size_t>(produced);
       d_status[0] =
           d_overflow[0] != 0 || produced > max_matches_u32 ? PGACCEL_UNSUPPORTED : PGACCEL_OK;
     }).wait_and_throw();
    pgaccel_record_gpu_exec();

    pgaccel_status status = PGACCEL_ERROR;
    q->memcpy(match_count, d_final_match_count, sizeof(size_t)).wait_and_throw();
    q->memcpy(&status, d_status, sizeof(pgaccel_status)).wait_and_throw();
    if (pair_u32s > 0)
      q->memcpy(match_pairs, d_pairs, pair_u32s * sizeof(uint32_t)).wait_and_throw();

    sycl::free(d_outer_keys, *q);
    if (d_outer_nulls != nullptr)
      sycl::free(d_outer_nulls, *q);
    if (d_pairs != nullptr)
      sycl::free(d_pairs, *q);
    sycl::free(d_match_count, *q);
    sycl::free(d_overflow, *q);
    sycl::free(d_final_match_count, *q);
    sycl::free(d_status, *q);

    return status;
  } catch (const sycl::exception& e) {
    std::fprintf(stderr, "pgaccel: hash_join probe kernel failed: %s\n", e.what());
  } catch (const std::exception& e) {
    std::fprintf(stderr, "pgaccel: hash_join probe failed: %s\n", e.what());
  }

  if (d_outer_keys != nullptr)
    sycl::free(d_outer_keys, *q);
  if (d_outer_nulls != nullptr)
    sycl::free(d_outer_nulls, *q);
  if (d_pairs != nullptr)
    sycl::free(d_pairs, *q);
  if (d_match_count != nullptr)
    sycl::free(d_match_count, *q);
  if (d_overflow != nullptr)
    sycl::free(d_overflow, *q);
  if (d_final_match_count != nullptr)
    sycl::free(d_final_match_count, *q);
  if (d_status != nullptr)
    sycl::free(d_status, *q);
  return PGACCEL_ERROR_NO_DEVICE;
}

template <typename K>
static pgaccel_status count_typed(const pgaccel_hash_table* ht, const K* outer_keys,
                                  const uint8_t* outer_null_mask, size_t outer_count,
                                  size_t* match_count) {
  if (ht == nullptr || outer_keys == nullptr || match_count == nullptr) {
    return PGACCEL_ERROR;
  }
  std::memset(match_count, 0, sizeof(*match_count));
  if (outer_count == 0) {
    return PGACCEL_OK;
  }
  if (outer_count > static_cast<size_t>(std::numeric_limits<uint32_t>::max())) {
    return PGACCEL_UNSUPPORTED;
  }
  if (!has_count_storage(ht)) {
    return PGACCEL_UNSUPPORTED;
  }

  sycl::queue* q = get_queue();
  if (q == nullptr || ht->queue == nullptr || q != ht->queue) {
    return PGACCEL_ERROR_NO_DEVICE;
  }

  const size_t alloc_outer = outer_count;
  K* d_outer_keys = nullptr;
  uint8_t* d_outer_nulls = nullptr;
  uint32_t* d_row_counts = nullptr;
  size_t* d_final_match_count = nullptr;
  pgaccel_status* d_status = nullptr;

  try {
    d_outer_keys = sycl::malloc_device<K>(alloc_outer, *q);
    if (outer_null_mask != nullptr) {
      d_outer_nulls = sycl::malloc_device<uint8_t>(alloc_outer, *q);
    }
    d_row_counts = sycl::malloc_device<uint32_t>(alloc_outer, *q);
    d_final_match_count = sycl::malloc_device<size_t>(1, *q);
    d_status = sycl::malloc_device<pgaccel_status>(1, *q);

    if (d_outer_keys == nullptr || (outer_null_mask != nullptr && d_outer_nulls == nullptr) ||
        d_row_counts == nullptr || d_final_match_count == nullptr || d_status == nullptr) {
      if (d_outer_keys != nullptr)
        sycl::free(d_outer_keys, *q);
      if (d_outer_nulls != nullptr)
        sycl::free(d_outer_nulls, *q);
      if (d_row_counts != nullptr)
        sycl::free(d_row_counts, *q);
      if (d_final_match_count != nullptr)
        sycl::free(d_final_match_count, *q);
      if (d_status != nullptr)
        sycl::free(d_status, *q);
      return PGACCEL_OOM;
    }

    q->memcpy(d_outer_keys, outer_keys, outer_count * sizeof(K));
    if (outer_null_mask != nullptr) {
      q->memcpy(d_outer_nulls, outer_null_mask, outer_count * sizeof(uint8_t));
    }
    q->wait_and_throw();

    const K* d_build_keys = static_cast<const K*>(ht->d_keys);
    const int32_t* d_heads = ht->d_heads;
    const int32_t* d_next = ht->d_next;
    const size_t table_capacity = ht->capacity;
    const size_t mask = table_capacity - 1;

    q->parallel_for(sycl::range<1>(outer_count), [=](sycl::id<1> id) {
       const size_t outer_row = id[0];
       d_row_counts[outer_row] = 0;
       if (d_outer_nulls != nullptr && d_outer_nulls[outer_row] != 0) {
         return;
       }

       const K key = d_outer_keys[outer_row];
       const uint64_t h = hash_key<K>(key);

       for (size_t attempt = 0; attempt < table_capacity; ++attempt) {
         const size_t slot = (h + attempt) & mask;
         const int32_t head = d_heads[slot];
         if (head == EMPTY_HEAD) {
           return;
         }
         if (d_build_keys[static_cast<size_t>(head)] != key) {
           continue;
         }

         int32_t cur = head;
         uint32_t local_count = 0;
         while (cur != EMPTY_HEAD) {
           if (local_count != std::numeric_limits<uint32_t>::max())
             local_count += 1u;
           cur = d_next[static_cast<size_t>(cur)];
         }
         d_row_counts[outer_row] = local_count;
         return;
       }
     }).wait_and_throw();

    q->single_task([=]() {
       size_t produced = 0;
       uint32_t overflow = 0;
       for (size_t row = 0; row < outer_count; ++row) {
         const size_t count = static_cast<size_t>(d_row_counts[row]);
         if (count > std::numeric_limits<size_t>::max() - produced) {
           overflow = 1;
           break;
         }
         produced += count;
       }
       d_final_match_count[0] = produced;
       d_status[0] = overflow != 0 ? PGACCEL_UNSUPPORTED : PGACCEL_OK;
     }).wait_and_throw();
    pgaccel_record_gpu_exec();

    pgaccel_status status = PGACCEL_ERROR;
    q->memcpy(match_count, d_final_match_count, sizeof(size_t)).wait_and_throw();
    q->memcpy(&status, d_status, sizeof(pgaccel_status)).wait_and_throw();

    sycl::free(d_outer_keys, *q);
    if (d_outer_nulls != nullptr)
      sycl::free(d_outer_nulls, *q);
    sycl::free(d_row_counts, *q);
    sycl::free(d_final_match_count, *q);
    sycl::free(d_status, *q);

    return status;
  } catch (const sycl::exception& e) {
    std::fprintf(stderr, "pgaccel: hash_join count kernel failed: %s\n", e.what());
  } catch (const std::exception& e) {
    std::fprintf(stderr, "pgaccel: hash_join count failed: %s\n", e.what());
  }

  if (d_outer_keys != nullptr)
    sycl::free(d_outer_keys, *q);
  if (d_outer_nulls != nullptr)
    sycl::free(d_outer_nulls, *q);
  if (d_row_counts != nullptr)
    sycl::free(d_row_counts, *q);
  if (d_final_match_count != nullptr)
    sycl::free(d_final_match_count, *q);
  if (d_status != nullptr)
    sycl::free(d_status, *q);
  return PGACCEL_ERROR_NO_DEVICE;
}

template <typename K>
static pgaccel_status count_device_typed(const pgaccel_hash_table* ht, const K* device_outer_keys,
                                         const uint8_t* device_outer_null_mask, size_t outer_count,
                                         size_t* match_count) {
  if (ht == nullptr || device_outer_keys == nullptr || match_count == nullptr) {
    return PGACCEL_ERROR;
  }
  std::memset(match_count, 0, sizeof(*match_count));
  if (outer_count == 0) {
    return PGACCEL_OK;
  }
  if (outer_count > static_cast<size_t>(std::numeric_limits<uint32_t>::max())) {
    return PGACCEL_UNSUPPORTED;
  }
  if (!has_count_storage(ht)) {
    return PGACCEL_UNSUPPORTED;
  }

  sycl::queue* q = get_queue();
  if (q == nullptr || ht->queue == nullptr || q != ht->queue) {
    return PGACCEL_ERROR_NO_DEVICE;
  }

  uint32_t* d_row_counts = nullptr;
  size_t* d_final_match_count = nullptr;
  pgaccel_status* d_status = nullptr;
  try {
    d_row_counts = sycl::malloc_device<uint32_t>(outer_count, *q);
    d_final_match_count = sycl::malloc_device<size_t>(1, *q);
    d_status = sycl::malloc_device<pgaccel_status>(1, *q);
    if (d_row_counts == nullptr || d_final_match_count == nullptr || d_status == nullptr) {
      if (d_row_counts != nullptr)
        sycl::free(d_row_counts, *q);
      if (d_final_match_count != nullptr)
        sycl::free(d_final_match_count, *q);
      if (d_status != nullptr)
        sycl::free(d_status, *q);
      return PGACCEL_OOM;
    }

    const K* d_outer_keys = device_outer_keys;
    const uint8_t* d_outer_nulls = device_outer_null_mask;
    const K* d_build_keys = static_cast<const K*>(ht->d_keys);
    const int32_t* d_heads = ht->d_heads;
    const int32_t* d_next = ht->d_next;
    const size_t table_capacity = ht->capacity;
    const size_t mask = table_capacity - 1;

    q->parallel_for(sycl::range<1>(outer_count), [=](sycl::id<1> id) {
       const size_t outer_row = id[0];
       d_row_counts[outer_row] = 0;
       if (d_outer_nulls != nullptr && d_outer_nulls[outer_row] != 0) {
         return;
       }

       const K key = d_outer_keys[outer_row];
       const uint64_t h = hash_key<K>(key);

       for (size_t attempt = 0; attempt < table_capacity; ++attempt) {
         const size_t slot = (h + attempt) & mask;
         const int32_t head = d_heads[slot];
         if (head == EMPTY_HEAD) {
           return;
         }
         if (d_build_keys[static_cast<size_t>(head)] != key) {
           continue;
         }

         uint32_t local_count = 0;
         int32_t cur = head;
         while (cur != EMPTY_HEAD) {
           if (local_count != std::numeric_limits<uint32_t>::max())
             local_count += 1u;
           cur = d_next[static_cast<size_t>(cur)];
         }
         d_row_counts[outer_row] = local_count;
         return;
       }
     }).wait_and_throw();

    q->single_task([=]() {
       size_t produced = 0;
       uint32_t overflow = 0;
       for (size_t row = 0; row < outer_count; ++row) {
         const size_t count = static_cast<size_t>(d_row_counts[row]);
         if (count > std::numeric_limits<size_t>::max() - produced) {
           overflow = 1;
           break;
         }
         produced += count;
       }
       d_final_match_count[0] = produced;
       d_status[0] = overflow != 0 ? PGACCEL_UNSUPPORTED : PGACCEL_OK;
     }).wait_and_throw();
    pgaccel_record_gpu_exec();

    pgaccel_status status = PGACCEL_ERROR;
    q->memcpy(match_count, d_final_match_count, sizeof(size_t)).wait_and_throw();
    q->memcpy(&status, d_status, sizeof(pgaccel_status)).wait_and_throw();
    sycl::free(d_row_counts, *q);
    sycl::free(d_final_match_count, *q);
    sycl::free(d_status, *q);
    return status;
  } catch (const sycl::exception& e) {
    std::fprintf(stderr, "pgaccel: resident hash_join count kernel failed: %s\n", e.what());
  } catch (const std::exception& e) {
    std::fprintf(stderr, "pgaccel: resident hash_join count failed: %s\n", e.what());
  }

  if (d_row_counts != nullptr)
    sycl::free(d_row_counts, *q);
  if (d_final_match_count != nullptr)
    sycl::free(d_final_match_count, *q);
  if (d_status != nullptr)
    sycl::free(d_status, *q);
  return PGACCEL_ERROR_NO_DEVICE;
}

}  // namespace

extern "C" {

pgaccel_hash_table* pgaccel_hash_join_build(const void* keys, const uint8_t* null_mask,
                                            const uint32_t* indices, size_t count,
                                            pgaccel_key_type key_type) {
  if (keys == nullptr || indices == nullptr || count == 0 || key_size(key_type) == 0) {
    return nullptr;
  }

  if (key_type == PGACCEL_KEY_INT32)
    return build_typed<int32_t>(static_cast<const int32_t*>(keys), null_mask, indices, count,
                                key_type);
  if (key_type == PGACCEL_KEY_INT64)
    return build_typed<int64_t>(static_cast<const int64_t*>(keys), null_mask, indices, count,
                                key_type);
  return nullptr;
}

pgaccel_hash_table* pgaccel_hash_join_build_device_count(const void* device_keys,
                                                         const uint8_t* device_null_mask,
                                                         size_t count, pgaccel_key_type key_type) {
  if (device_keys == nullptr || count == 0 || key_size(key_type) == 0) {
    return nullptr;
  }

  if (key_type == PGACCEL_KEY_INT32)
    return build_device_count_typed<int32_t>(static_cast<const int32_t*>(device_keys),
                                             device_null_mask, count, key_type);
  if (key_type == PGACCEL_KEY_INT64)
    return build_device_count_typed<int64_t>(static_cast<const int64_t*>(device_keys),
                                             device_null_mask, count, key_type);
  return nullptr;
}

void pgaccel_hash_join_free(pgaccel_hash_table* ht) try {
  if (ht == nullptr) {
    return;
  }
  free_table_storage(ht);
  delete ht;
} catch (const std::exception& e) {
  std::fprintf(stderr, "pgaccel: pgaccel_hash_join_free failed: %s\n", e.what());
} catch (...) {
  std::fprintf(stderr, "pgaccel: pgaccel_hash_join_free failed: unknown C++ exception\n");
}

pgaccel_status pgaccel_hash_join_probe(const pgaccel_hash_table* ht, const void* outer_keys,
                                       const uint8_t* outer_null_mask, size_t outer_count,
                                       uint32_t* match_pairs, size_t max_matches,
                                       size_t* match_count) try {
  if (ht == nullptr || outer_keys == nullptr || match_count == nullptr) {
    return PGACCEL_ERROR;
  }

  if (ht->key_type == PGACCEL_KEY_INT32)
    return probe_typed<int32_t>(ht, static_cast<const int32_t*>(outer_keys), outer_null_mask,
                                outer_count, match_pairs, max_matches, match_count);
  if (ht->key_type == PGACCEL_KEY_INT64)
    return probe_typed<int64_t>(ht, static_cast<const int64_t*>(outer_keys), outer_null_mask,
                                outer_count, match_pairs, max_matches, match_count);
  return PGACCEL_UNSUPPORTED;
} catch (const pgaccel_no_device_error&) {
  return PGACCEL_ERROR_NO_DEVICE;
} catch (const std::exception& e) {
  return pgaccel_kernel_failure("pgaccel_hash_join_probe", &e);
} catch (...) {
  return pgaccel_kernel_failure("pgaccel_hash_join_probe", nullptr);
}

pgaccel_status pgaccel_hash_join_count(const pgaccel_hash_table* ht, const void* outer_keys,
                                       const uint8_t* outer_null_mask, size_t outer_count,
                                       size_t* match_count) try {
  if (ht == nullptr || outer_keys == nullptr || match_count == nullptr) {
    return PGACCEL_ERROR;
  }

  if (ht->key_type == PGACCEL_KEY_INT32)
    return count_typed<int32_t>(ht, static_cast<const int32_t*>(outer_keys), outer_null_mask,
                                outer_count, match_count);
  if (ht->key_type == PGACCEL_KEY_INT64)
    return count_typed<int64_t>(ht, static_cast<const int64_t*>(outer_keys), outer_null_mask,
                                outer_count, match_count);
  return PGACCEL_UNSUPPORTED;
} catch (const pgaccel_no_device_error&) {
  return PGACCEL_ERROR_NO_DEVICE;
} catch (const std::exception& e) {
  return pgaccel_kernel_failure("pgaccel_hash_join_count", &e);
} catch (...) {
  return pgaccel_kernel_failure("pgaccel_hash_join_count", nullptr);
}

pgaccel_status pgaccel_hash_join_count_device(const pgaccel_hash_table* ht,
                                              const void* device_outer_keys,
                                              const uint8_t* device_outer_null_mask,
                                              size_t outer_count, size_t* match_count) try {
  if (ht == nullptr || device_outer_keys == nullptr || match_count == nullptr) {
    return PGACCEL_ERROR;
  }

  if (ht->key_type == PGACCEL_KEY_INT32)
    return count_device_typed<int32_t>(ht, static_cast<const int32_t*>(device_outer_keys),
                                       device_outer_null_mask, outer_count, match_count);
  if (ht->key_type == PGACCEL_KEY_INT64)
    return count_device_typed<int64_t>(ht, static_cast<const int64_t*>(device_outer_keys),
                                       device_outer_null_mask, outer_count, match_count);
  return PGACCEL_UNSUPPORTED;
} catch (const pgaccel_no_device_error&) {
  return PGACCEL_ERROR_NO_DEVICE;
} catch (const std::exception& e) {
  return pgaccel_kernel_failure("pgaccel_hash_join_count_device", &e);
} catch (...) {
  return pgaccel_kernel_failure("pgaccel_hash_join_count_device", nullptr);
}

}  // extern "C"

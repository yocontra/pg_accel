/*
 * hash_join.cpp -- resident count-only GPU hash join.
 *
 * General row-emitting joins are intentionally outside the product: copying
 * `(outer_row, inner_row)` pairs back through PostgreSQL is structurally slower
 * than the native executor.  This file therefore accepts only resident INT32
 * and INT64 key buffers and returns one aggregate match count.
 */

#include <sycl/sycl.hpp>

#include <cstdint>
#include <cstdio>
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
  const void* device_keys;
  const uint8_t* device_null_mask;
  int32_t* device_heads;
  int32_t* device_next;
};

namespace {

constexpr int32_t kEmptyHead = -1;

inline uint64_t hash64(uint64_t key) {
  key ^= key >> 33;
  key *= 0xff51afd7ed558ccdULL;
  key ^= key >> 33;
  key *= 0xc4ceb9fe1a85ec53ULL;
  key ^= key >> 33;
  return key;
}

template <typename Key>
inline uint64_t hash_key(Key key) {
  if constexpr (sizeof(Key) == sizeof(uint32_t)) {
    return hash64(static_cast<uint64_t>(static_cast<uint32_t>(key)));
  }
  return hash64(static_cast<uint64_t>(key));
}

bool next_power_of_two_checked(size_t value, size_t* out) {
  if (out == nullptr)
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

bool hash_join_capacity(size_t rows, size_t* out_capacity) {
  if (out_capacity == nullptr || rows > std::numeric_limits<size_t>::max() / 2)
    return false;
  size_t requested = rows * 2;
  if (requested < 2)
    requested = 2;

  size_t rounded = 0;
  if (!next_power_of_two_checked(requested, &rounded))
    return false;
  if (rounded < 16)
    rounded = 16;
  if (rounded > static_cast<size_t>(std::numeric_limits<int32_t>::max()))
    return false;
  *out_capacity = rounded;
  return true;
}

size_t key_size(pgaccel_key_type key_type) {
  switch (key_type) {
    case PGACCEL_KEY_INT32:
      return sizeof(int32_t);
    case PGACCEL_KEY_INT64:
      return sizeof(int64_t);
    default:
      return 0;
  }
}

bool is_metal_backend() {
  const pgaccel_platform_caps caps = pgaccel_get_caps();
  return std::strcmp(caps.backend_name, "metal") == 0;
}

bool has_count_storage(const pgaccel_hash_table* table) {
  return table != nullptr && table->device_keys != nullptr && table->device_heads != nullptr &&
         table->device_next != nullptr && table->capacity != 0 &&
         (table->capacity & (table->capacity - 1)) == 0;
}

void free_table_storage(pgaccel_hash_table* table) {
  if (table == nullptr || table->queue == nullptr)
    return;
  if (table->device_heads != nullptr)
    sycl::free(table->device_heads, *table->queue);
  if (table->device_next != nullptr)
    sycl::free(table->device_next, *table->queue);
  table->device_heads = nullptr;
  table->device_next = nullptr;
}

template <typename Key>
bool build_serial(sycl::queue& queue, const Key* keys, const uint8_t* nulls, int32_t* heads,
                  int32_t* next, size_t count, size_t capacity) {
  uint32_t* build_failed = sycl::malloc_shared<uint32_t>(1, queue);
  if (build_failed == nullptr)
    return false;
  *build_failed = 0;
  const size_t mask = capacity - 1;

  try {
    queue
        .single_task([=]() {
          for (size_t row = 0; row < count; ++row) {
            if (nulls != nullptr && nulls[row] != 0)
              continue;

            const Key key = keys[row];
            const uint64_t hash = hash_key<Key>(key);
            const int32_t row_index = static_cast<int32_t>(row);
            bool inserted = false;
            for (size_t attempt = 0; attempt < capacity; ++attempt) {
              const size_t slot = (hash + attempt) & mask;
              const int32_t head = heads[slot];
              if (head == kEmptyHead) {
                heads[slot] = row_index;
                next[row] = kEmptyHead;
                inserted = true;
                break;
              }
              if (keys[static_cast<size_t>(head)] != key)
                continue;
              next[row] = head;
              heads[slot] = row_index;
              inserted = true;
              break;
            }
            if (!inserted) {
              *build_failed = 1;
              return;
            }
          }
        })
        .wait_and_throw();
    const bool built = *build_failed == 0;
    sycl::free(build_failed, queue);
    return built;
  } catch (...) {
    sycl::free(build_failed, queue);
    throw;
  }
}

template <typename Key>
void build_parallel(sycl::queue& queue, const Key* keys, const uint8_t* nulls, int32_t* heads,
                    int32_t* next, size_t count, size_t capacity) {
  const size_t mask = capacity - 1;
  queue
      .parallel_for(
          sycl::range<1>(count),
          [=](sycl::id<1> id) {
            const size_t row = id[0];
            if (nulls != nullptr && nulls[row] != 0)
              return;

            const Key key = keys[row];
            const uint64_t hash = hash_key<Key>(key);
            const int32_t row_index = static_cast<int32_t>(row);
            for (size_t attempt = 0; attempt < capacity; ++attempt) {
              const size_t slot = (hash + attempt) & mask;
              sycl::atomic_ref<int32_t, sycl::memory_order::acq_rel, sycl::memory_scope::device,
                               sycl::access::address_space::global_space>
                  head_ref(heads[slot]);

              int32_t expected = kEmptyHead;
              if (head_ref.compare_exchange_strong(expected, row_index)) {
                next[row] = kEmptyHead;
                return;
              }
              int32_t head = expected;
              if (keys[static_cast<size_t>(head)] != key)
                continue;

              for (;;) {
                next[row] = head;
                int32_t compare = head;
                if (head_ref.compare_exchange_strong(compare, row_index))
                  return;
                head = compare;
              }
            }
          })
      .wait_and_throw();
}

template <typename Key>
pgaccel_hash_table* build_device_count_typed(const Key* device_keys,
                                             const uint8_t* device_null_mask, size_t count,
                                             pgaccel_key_type key_type) {
  if (device_keys == nullptr || count == 0 ||
      count > static_cast<size_t>(std::numeric_limits<int32_t>::max())) {
    return nullptr;
  }

  sycl::queue* queue = pgaccel_get_queue();
  if (queue == nullptr)
    return nullptr;

  size_t capacity = 0;
  if (!hash_join_capacity(count, &capacity))
    return nullptr;

  int32_t* heads = nullptr;
  int32_t* next = nullptr;
  auto cleanup = [&]() {
    if (heads != nullptr)
      sycl::free(heads, *queue);
    if (next != nullptr)
      sycl::free(next, *queue);
  };

  try {
    next = sycl::malloc_device<int32_t>(count, *queue);
    heads = sycl::malloc_device<int32_t>(capacity, *queue);
    if (next == nullptr || heads == nullptr) {
      cleanup();
      return nullptr;
    }
    queue->fill(next, kEmptyHead, count);
    queue->fill(heads, kEmptyHead, capacity);
    queue->wait_and_throw();

    bool built = true;
    if (is_metal_backend()) {
      built = build_serial(*queue, device_keys, device_null_mask, heads, next, count, capacity);
    } else {
      build_parallel(*queue, device_keys, device_null_mask, heads, next, count, capacity);
    }
    if (!built) {
      cleanup();
      return nullptr;
    }

    auto* table = new (std::nothrow) pgaccel_hash_table{};
    if (table == nullptr) {
      cleanup();
      return nullptr;
    }
    table->key_type = key_type;
    table->count = count;
    table->capacity = capacity;
    table->queue = queue;
    table->device_keys = device_keys;
    table->device_null_mask = device_null_mask;
    table->device_heads = heads;
    table->device_next = next;
    pgaccel_record_gpu_exec();
    return table;
  } catch (const sycl::exception& error) {
    std::fprintf(stderr, "pgaccel: resident hash join build failed: %s\n", error.what());
  } catch (const std::exception& error) {
    std::fprintf(stderr, "pgaccel: resident hash join build failed: %s\n", error.what());
  }
  cleanup();
  return nullptr;
}

template <typename Key>
pgaccel_status count_device_typed(const pgaccel_hash_table* table, const Key* outer_keys,
                                  const uint8_t* outer_null_mask, size_t outer_count,
                                  size_t* match_count) {
  if (table == nullptr || outer_keys == nullptr || match_count == nullptr)
    return PGACCEL_ERROR;
  *match_count = 0;
  if (outer_count == 0)
    return PGACCEL_OK;
  if (outer_count > static_cast<size_t>(std::numeric_limits<uint32_t>::max()))
    return PGACCEL_UNSUPPORTED;
  if (!has_count_storage(table))
    return PGACCEL_UNSUPPORTED;

  sycl::queue* queue = pgaccel_get_queue();
  if (queue == nullptr || table->queue == nullptr || queue != table->queue)
    return PGACCEL_ERROR_NO_DEVICE;

  uint32_t* row_counts = nullptr;
  size_t* final_count = nullptr;
  pgaccel_status* status = nullptr;
  auto cleanup = [&]() {
    if (row_counts != nullptr)
      sycl::free(row_counts, *queue);
    if (final_count != nullptr)
      sycl::free(final_count, *queue);
    if (status != nullptr)
      sycl::free(status, *queue);
  };

  try {
    row_counts = sycl::malloc_device<uint32_t>(outer_count, *queue);
    final_count = sycl::malloc_device<size_t>(1, *queue);
    status = sycl::malloc_device<pgaccel_status>(1, *queue);
    if (row_counts == nullptr || final_count == nullptr || status == nullptr) {
      cleanup();
      return PGACCEL_OOM;
    }

    const Key* build_keys = static_cast<const Key*>(table->device_keys);
    const int32_t* heads = table->device_heads;
    const int32_t* next = table->device_next;
    const size_t capacity = table->capacity;
    const size_t mask = capacity - 1;

    queue
        ->parallel_for(sycl::range<1>(outer_count),
                       [=](sycl::id<1> id) {
                         const size_t outer_row = id[0];
                         row_counts[outer_row] = 0;
                         if (outer_null_mask != nullptr && outer_null_mask[outer_row] != 0)
                           return;

                         const Key key = outer_keys[outer_row];
                         const uint64_t hash = hash_key<Key>(key);
                         for (size_t attempt = 0; attempt < capacity; ++attempt) {
                           const size_t slot = (hash + attempt) & mask;
                           const int32_t head = heads[slot];
                           if (head == kEmptyHead)
                             return;
                           if (build_keys[static_cast<size_t>(head)] != key)
                             continue;

                           uint32_t local_count = 0;
                           int32_t current = head;
                           while (current != kEmptyHead) {
                             if (local_count != std::numeric_limits<uint32_t>::max())
                               ++local_count;
                             current = next[static_cast<size_t>(current)];
                           }
                           row_counts[outer_row] = local_count;
                           return;
                         }
                       })
        .wait_and_throw();

    queue
        ->single_task([=]() {
          size_t produced = 0;
          uint32_t overflow = 0;
          for (size_t row = 0; row < outer_count; ++row) {
            const size_t count = static_cast<size_t>(row_counts[row]);
            if (count > std::numeric_limits<size_t>::max() - produced) {
              overflow = 1;
              break;
            }
            produced += count;
          }
          final_count[0] = produced;
          status[0] = overflow != 0 ? PGACCEL_UNSUPPORTED : PGACCEL_OK;
        })
        .wait_and_throw();
    pgaccel_record_gpu_exec();

    pgaccel_status host_status = PGACCEL_ERROR;
    queue->memcpy(match_count, final_count, sizeof(*match_count)).wait_and_throw();
    queue->memcpy(&host_status, status, sizeof(host_status)).wait_and_throw();
    cleanup();
    return host_status;
  } catch (const sycl::exception& error) {
    std::fprintf(stderr, "pgaccel: resident hash join count failed: %s\n", error.what());
  } catch (const std::exception& error) {
    std::fprintf(stderr, "pgaccel: resident hash join count failed: %s\n", error.what());
  }
  cleanup();
  return PGACCEL_ERROR_NO_DEVICE;
}

}  // namespace

extern "C" {

pgaccel_hash_table* pgaccel_hash_join_build_device_count(const void* device_keys,
                                                         const uint8_t* device_null_mask,
                                                         size_t count, pgaccel_key_type key_type) {
  if (device_keys == nullptr || count == 0 || key_size(key_type) == 0)
    return nullptr;
  if (key_type == PGACCEL_KEY_INT32) {
    return build_device_count_typed(static_cast<const int32_t*>(device_keys), device_null_mask,
                                    count, key_type);
  }
  if (key_type == PGACCEL_KEY_INT64) {
    return build_device_count_typed(static_cast<const int64_t*>(device_keys), device_null_mask,
                                    count, key_type);
  }
  return nullptr;
}

void pgaccel_hash_join_free(pgaccel_hash_table* table) try {
  if (table == nullptr)
    return;
  free_table_storage(table);
  delete table;
} catch (const std::exception& error) {
  std::fprintf(stderr, "pgaccel: pgaccel_hash_join_free failed: %s\n", error.what());
} catch (...) {
  std::fprintf(stderr, "pgaccel: pgaccel_hash_join_free failed: unknown C++ exception\n");
}

pgaccel_status pgaccel_hash_join_count_device(const pgaccel_hash_table* table,
                                              const void* device_outer_keys,
                                              const uint8_t* device_outer_null_mask,
                                              size_t outer_count, size_t* match_count) try {
  if (table == nullptr || device_outer_keys == nullptr || match_count == nullptr)
    return PGACCEL_ERROR;
  if (table->key_type == PGACCEL_KEY_INT32) {
    return count_device_typed(table, static_cast<const int32_t*>(device_outer_keys),
                              device_outer_null_mask, outer_count, match_count);
  }
  if (table->key_type == PGACCEL_KEY_INT64) {
    return count_device_typed(table, static_cast<const int64_t*>(device_outer_keys),
                              device_outer_null_mask, outer_count, match_count);
  }
  return PGACCEL_UNSUPPORTED;
} catch (const pgaccel_no_device_error&) {
  return PGACCEL_ERROR_NO_DEVICE;
} catch (const std::exception& error) {
  return pgaccel_kernel_failure("pgaccel_hash_join_count_device", &error);
} catch (...) {
  return pgaccel_kernel_failure("pgaccel_hash_join_count_device", nullptr);
}

}  // extern "C"

/*
 * resident_count.cpp - bounded resident int64 grouped COUNT(*).
 *
 * This is the only survivor of the former standalone hash-aggregation engine.
 * Input keys stay device-accessible through grouping and compaction. There is
 * no host hash table, host grouping fallback, sort path, or partial/finalize
 * aggregate machinery in this translation unit.
 */

#include <sycl/sycl.hpp>

#include <algorithm>
#include <cstdint>
#include <cstdio>
#include <limits>
#include <new>
#include <vector>

#include "pgaccel_queue.h"
#include "pgaccel_resident_count.h"

struct pgaccel_agg_state {
  std::vector<int64_t> group_keys;
  std::vector<int64_t> counts;
  std::vector<double> results;
};

namespace {

constexpr uint32_t kEmptyOwner = std::numeric_limits<uint32_t>::max();

uint64_t hash64(uint64_t value) {
  value ^= value >> 33;
  value *= 0xff51afd7ed558ccdULL;
  value ^= value >> 33;
  value *= 0xc4ceb9fe1a85ec53ULL;
  value ^= value >> 33;
  return value;
}

bool checked_add(size_t left, size_t right, size_t* out) {
  if (out == nullptr || left > std::numeric_limits<size_t>::max() - right)
    return false;
  *out = left + right;
  return true;
}

bool checked_mul(size_t left, size_t right, size_t* out) {
  if (out == nullptr || (left != 0 && right > std::numeric_limits<size_t>::max() / left))
    return false;
  *out = left * right;
  return true;
}

bool align_up(size_t value, size_t alignment, size_t* out) {
  if (out == nullptr || alignment == 0 || (alignment & (alignment - 1)) != 0)
    return false;
  const size_t remainder = value & (alignment - 1);
  if (remainder == 0) {
    *out = value;
    return true;
  }
  return checked_add(value, alignment - remainder, out);
}

bool next_power_of_two(size_t value, size_t* out) {
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

struct ResidentCountSlabLayout {
  size_t owners_offset = 0;
  size_t counts_offset = 0;
  size_t output_keys_offset = 0;
  size_t output_counts_offset = 0;
  size_t output_results_offset = 0;
  size_t group_count_offset = 0;
  size_t overflow_offset = 0;
  size_t bytes = 0;
};

bool append_region(size_t count, size_t element_size, size_t alignment, size_t* cursor,
                   size_t* offset) {
  size_t aligned = 0;
  size_t span = 0;
  size_t next = 0;
  if (cursor == nullptr || offset == nullptr || !align_up(*cursor, alignment, &aligned) ||
      !checked_mul(count, element_size, &span) || !checked_add(aligned, span, &next)) {
    return false;
  }
  *offset = aligned;
  *cursor = next;
  return true;
}

bool make_slab_layout(size_t table_capacity, size_t max_distinct, ResidentCountSlabLayout* layout) {
  if (layout == nullptr)
    return false;
  size_t cursor = 0;
  if (!append_region(table_capacity, sizeof(uint32_t), alignof(uint32_t), &cursor,
                     &layout->owners_offset) ||
      !append_region(table_capacity, sizeof(uint32_t), alignof(uint32_t), &cursor,
                     &layout->counts_offset) ||
      !append_region(max_distinct, sizeof(int64_t), alignof(int64_t), &cursor,
                     &layout->output_keys_offset) ||
      !append_region(max_distinct, sizeof(int64_t), alignof(int64_t), &cursor,
                     &layout->output_counts_offset) ||
      !append_region(max_distinct, sizeof(double), alignof(double), &cursor,
                     &layout->output_results_offset) ||
      !append_region(1, sizeof(uint32_t), alignof(uint32_t), &cursor,
                     &layout->group_count_offset) ||
      !append_region(1, sizeof(uint32_t), alignof(uint32_t), &cursor, &layout->overflow_offset)) {
    return false;
  }
  layout->bytes = cursor;
  return true;
}

pgaccel_status execute_bounded(int64_t* group_keys, size_t row_count, size_t max_distinct_hint,
                               pgaccel_agg_state** out_state) {
  if (out_state == nullptr)
    return PGACCEL_INVALID_ARGUMENT;
  *out_state = nullptr;

  if (row_count == 0) {
    auto* empty = new (std::nothrow) pgaccel_agg_state();
    if (empty == nullptr)
      return PGACCEL_OOM;
    *out_state = empty;
    return PGACCEL_OK;
  }
  if (group_keys == nullptr)
    return PGACCEL_INVALID_ARGUMENT;
  if (row_count > static_cast<size_t>(std::numeric_limits<uint32_t>::max()))
    return PGACCEL_UNSUPPORTED;

  size_t max_distinct = max_distinct_hint;
  if (max_distinct == 0 || max_distinct > row_count)
    max_distinct = row_count;

  size_t table_need = 0;
  size_t table_capacity = 0;
  if (!checked_mul(max_distinct, 2, &table_need) ||
      !next_power_of_two(std::max<size_t>(table_need, 2), &table_capacity) ||
      table_capacity > static_cast<size_t>(std::numeric_limits<uint32_t>::max())) {
    return PGACCEL_UNSUPPORTED;
  }

  sycl::queue* queue = pgaccel_get_queue();
  if (queue == nullptr)
    return PGACCEL_ERROR_NO_DEVICE;

  ResidentCountSlabLayout layout;
  if (!make_slab_layout(table_capacity, max_distinct, &layout))
    return PGACCEL_OOM;

  uint8_t* slab = sycl::malloc_shared<uint8_t>(layout.bytes, *queue);
  if (slab == nullptr)
    return PGACCEL_OOM;

  auto cleanup = [&]() { sycl::free(slab, *queue); };
  auto* owners = reinterpret_cast<uint32_t*>(slab + layout.owners_offset);
  auto* slot_counts = reinterpret_cast<uint32_t*>(slab + layout.counts_offset);
  auto* output_keys = reinterpret_cast<int64_t*>(slab + layout.output_keys_offset);
  auto* output_counts = reinterpret_cast<int64_t*>(slab + layout.output_counts_offset);
  auto* output_results = reinterpret_cast<double*>(slab + layout.output_results_offset);
  auto* group_count = reinterpret_cast<uint32_t*>(slab + layout.group_count_offset);
  auto* overflow = reinterpret_cast<uint32_t*>(slab + layout.overflow_offset);

  queue->fill(owners, kEmptyOwner, table_capacity).wait_and_throw();
  queue->fill(slot_counts, 0u, table_capacity).wait_and_throw();
  queue->fill(group_count, 0u, 1).wait_and_throw();
  queue->fill(overflow, 0u, 1).wait_and_throw();

  const uint32_t table_mask = static_cast<uint32_t>(table_capacity - 1);
  queue
      ->parallel_for(
          sycl::range<1>(row_count),
          [=](sycl::id<1> id) {
            const uint32_t row = static_cast<uint32_t>(id[0]);
            const int64_t key = group_keys[row];
            uint32_t slot = static_cast<uint32_t>(hash64(static_cast<uint64_t>(key))) & table_mask;

            for (uint32_t probe = 0; probe <= table_mask; ++probe) {
              sycl::atomic_ref<uint32_t, sycl::memory_order::relaxed, sycl::memory_scope::device,
                               sycl::access::address_space::global_space>
                  owner_ref(owners[slot]);
              uint32_t owner = owner_ref.load();
              if (owner == kEmptyOwner) {
                uint32_t expected = kEmptyOwner;
                if (owner_ref.compare_exchange_strong(expected, row)) {
                  sycl::atomic_ref<uint32_t, sycl::memory_order::relaxed,
                                   sycl::memory_scope::device,
                                   sycl::access::address_space::global_space>
                      count_ref(slot_counts[slot]);
                  count_ref.fetch_add(1u);
                  return;
                }
                owner = expected;
              }

              if (owner != kEmptyOwner && group_keys[owner] == key) {
                sycl::atomic_ref<uint32_t, sycl::memory_order::relaxed, sycl::memory_scope::device,
                                 sycl::access::address_space::global_space>
                    count_ref(slot_counts[slot]);
                count_ref.fetch_add(1u);
                return;
              }
              slot = (slot + 1u) & table_mask;
            }

            sycl::atomic_ref<uint32_t, sycl::memory_order::relaxed, sycl::memory_scope::device,
                             sycl::access::address_space::global_space>
                overflow_ref(overflow[0]);
            overflow_ref.store(1u);
          })
      .wait_and_throw();
  pgaccel_record_gpu_exec();

  if (*overflow != 0) {
    cleanup();
    return PGACCEL_UNSUPPORTED;
  }

  const uint32_t distinct_limit = static_cast<uint32_t>(max_distinct);
  queue
      ->parallel_for(
          sycl::range<1>(table_capacity),
          [=](sycl::id<1> id) {
            const uint32_t slot = static_cast<uint32_t>(id[0]);
            const uint32_t owner = owners[slot];
            const uint32_t count = slot_counts[slot];
            if (owner == kEmptyOwner || count == 0)
              return;

            sycl::atomic_ref<uint32_t, sycl::memory_order::relaxed, sycl::memory_scope::device,
                             sycl::access::address_space::global_space>
                count_ref(group_count[0]);
            const uint32_t group = count_ref.fetch_add(1u);
            if (group >= distinct_limit) {
              sycl::atomic_ref<uint32_t, sycl::memory_order::relaxed, sycl::memory_scope::device,
                               sycl::access::address_space::global_space>
                  overflow_ref(overflow[0]);
              overflow_ref.store(1u);
              return;
            }

            output_keys[group] = group_keys[owner];
            output_counts[group] = static_cast<int64_t>(count);
            output_results[group] = static_cast<double>(count);
          })
      .wait_and_throw();
  pgaccel_record_gpu_exec();

  const size_t groups = *group_count;
  if (*overflow != 0 || groups == 0 || groups > max_distinct) {
    cleanup();
    return PGACCEL_UNSUPPORTED;
  }

  auto* state = new (std::nothrow) pgaccel_agg_state();
  if (state == nullptr) {
    cleanup();
    return PGACCEL_OOM;
  }

  try {
    state->group_keys.resize(groups);
    state->counts.resize(groups);
    state->results.resize(groups);
    queue->memcpy(state->group_keys.data(), output_keys, groups * sizeof(int64_t)).wait_and_throw();
    queue->memcpy(state->counts.data(), output_counts, groups * sizeof(int64_t)).wait_and_throw();
    queue->memcpy(state->results.data(), output_results, groups * sizeof(double)).wait_and_throw();
  } catch (...) {
    delete state;
    cleanup();
    throw;
  }

  cleanup();
  *out_state = state;
  return PGACCEL_OK;
}

}  // namespace

extern "C" pgaccel_status
pgaccel_hash_count_i64_device_hash_execute_bounded_checked(int64_t* group_keys, size_t row_count,
                                                           size_t max_distinct_hint,
                                                           pgaccel_agg_state** out_state) try {
  return execute_bounded(group_keys, row_count, max_distinct_hint, out_state);
} catch (const pgaccel_no_device_error&) {
  if (out_state != nullptr)
    *out_state = nullptr;
  return PGACCEL_ERROR_NO_DEVICE;
} catch (const std::bad_alloc&) {
  if (out_state != nullptr)
    *out_state = nullptr;
  return PGACCEL_OOM;
} catch (const std::exception& error) {
  if (out_state != nullptr)
    *out_state = nullptr;
  return pgaccel_kernel_failure("pgaccel_hash_count_i64_device_hash_execute_bounded_checked",
                                &error);
} catch (...) {
  if (out_state != nullptr)
    *out_state = nullptr;
  return pgaccel_kernel_failure("pgaccel_hash_count_i64_device_hash_execute_bounded_checked",
                                nullptr);
}

extern "C" pgaccel_agg_state*
pgaccel_hash_count_i64_device_hash_execute_bounded(int64_t* group_keys, size_t row_count,
                                                   size_t max_distinct_hint) {
  pgaccel_agg_state* state = nullptr;
  const pgaccel_status status = pgaccel_hash_count_i64_device_hash_execute_bounded_checked(
      group_keys, row_count, max_distinct_hint, &state);
  if (status != PGACCEL_OK)
    return nullptr;
  return state;
}

extern "C" size_t pgaccel_agg_group_count(const pgaccel_agg_state* state) {
  return state == nullptr ? 0 : state->group_keys.size();
}

extern "C" const void* pgaccel_agg_get_group_keys(const pgaccel_agg_state* state) {
  return state == nullptr || state->group_keys.empty() ? nullptr : state->group_keys.data();
}

extern "C" const double* pgaccel_agg_get_results(const pgaccel_agg_state* state, size_t agg_idx) {
  return state == nullptr || agg_idx != 0 || state->results.empty() ? nullptr
                                                                    : state->results.data();
}

extern "C" const int64_t* pgaccel_agg_get_counts(const pgaccel_agg_state* state) {
  return state == nullptr || state->counts.empty() ? nullptr : state->counts.data();
}

extern "C" void pgaccel_agg_free(pgaccel_agg_state* state) {
  delete state;
}

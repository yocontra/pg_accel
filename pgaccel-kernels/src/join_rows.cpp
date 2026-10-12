// Resident row mappings for composable join consumers. All row work is device work.
#include <sycl/sycl.hpp>

#include <algorithm>
#include <array>
#include <cstddef>
#include <cstdint>
#include <cstring>
#include <limits>

#include "pgaccel_join_rows.h"
#include "pgaccel_queue.h"
#include "join_rows_layout.hpp"

namespace {
using namespace pgaccel_join_detail;
constexpr int32_t kEmpty = -1;

static_assert(sizeof(pgaccel_join_rows_key) == 40);
static_assert(sizeof(pgaccel_join_rows_request) == 352);
static_assert(sizeof(pgaccel_join_row_pair) == 16);
static_assert(sizeof(pgaccel_join_rows_result) == 24);
static_assert(sizeof(pgaccel_join_control) == 40);
static_assert(sizeof(pgaccel_join_controlled_result) == 40);
static_assert(offsetof(pgaccel_join_control, cancel_check) == 24);
static_assert(offsetof(pgaccel_join_controlled_result, cancel_checks) == 32);
static_assert(offsetof(pgaccel_join_rows_key, type) == 32);
static_assert(offsetof(pgaccel_join_rows_request, outer_keys) == 32);
static_assert(offsetof(pgaccel_join_rows_request, inner_keys) == 192);
static_assert(offsetof(pgaccel_join_rows_result, kernel_launches) == 16);
static_assert(sizeof(pgaccel_join_project_column) == 72);
static_assert(sizeof(pgaccel_join_project_request) == 1208);
static_assert(sizeof(pgaccel_join_project_result) == 16);
static_assert(PGACCEL_JOIN_PROJECT_WORKSPACE_BYTES == 8 + PGACCEL_JOIN_PROJECT_MAX_COLUMNS * sizeof(pgaccel_join_project_column));
static_assert(offsetof(pgaccel_join_project_column, output_values) == 40);
static_assert(offsetof(pgaccel_join_project_request, columns) == 56);


bool accessible(sycl::queue& queue, const void* pointer) {
  if (pointer == nullptr)
    return true;
  try {
    const auto type = sycl::get_pointer_type(pointer, queue.get_context());
    return (type == sycl::usm::alloc::device || type == sycl::usm::alloc::shared) &&
           sycl::get_pointer_device(pointer, queue.get_context()) == queue.get_device();
  } catch (...) {
    return false;
  }
}

bool valid_aliases(const pgaccel_join_rows_request& request, const void* request_pointer,
                   void* workspace, uint64_t workspace_bytes, pgaccel_join_row_pair* output,
                   uint64_t capacity, void* result, uint64_t result_bytes) {
  if (capacity > SIZE_MAX / sizeof(*output) || !aligned(workspace, 8) ||
      !aligned(output, alignof(pgaccel_join_row_pair)) ||
      !aligned(result, alignof(pgaccel_join_rows_result)))
    return false;
  std::array<Span, 3> writes{};
  Span metadata;
  if (!span(workspace, workspace_bytes, &writes[0]) ||
      !span(output, capacity * sizeof(*output), &writes[1]) ||
      !span(result, result_bytes, &writes[2]) ||
      !span(request_pointer, sizeof(request), &metadata))
    return false;
  for (size_t i = 0; i < writes.size(); ++i) {
    if (overlaps(writes[i], metadata))
      return false;
    for (size_t j = i + 1; j < writes.size(); ++j)
      if (overlaps(writes[i], writes[j]))
        return false;
    for (size_t key = 0; key < request.key_count; ++key) {
      for (const auto* input : {&request.outer_keys[key], &request.inner_keys[key]}) {
        Span values, nulls;
        if (!span(input->values, input->value_bytes, &values) ||
            !span(input->nulls, input->null_bytes, &nulls) ||
            overlaps(writes[i], values) || overlaps(writes[i], nulls))
          return false;
      }
    }
  }
  return true;
}

uint64_t mix(uint64_t value) {
  value ^= value >> 33;
  value *= UINT64_C(0xff51afd7ed558ccd);
  value ^= value >> 33;
  value *= UINT64_C(0xc4ceb9fe1a85ec53);
  return value ^ (value >> 33);
}

uint64_t key_value(const pgaccel_join_rows_key& key, uint64_t row) {
  return key.type == PGACCEL_JOIN_ROWS_INT32
             ? static_cast<uint64_t>(static_cast<int64_t>(static_cast<const int32_t*>(key.values)[row]))
             : static_cast<const uint64_t*>(key.values)[row];
}

bool key_hash(const pgaccel_join_rows_key* keys, uint32_t count, uint64_t row,
              uint64_t* hash) {
  uint64_t value = UINT64_C(0x9e3779b97f4a7c15);
  for (uint32_t i = 0; i < count; ++i) {
    if (keys[i].nulls != nullptr && keys[i].nulls[row] != 0)
      return false;
    value = mix(value ^ mix(key_value(keys[i], row)) ^ i);
  }
  *hash = value;
  return true;
}

bool equal_keys(const pgaccel_join_rows_request& request, uint64_t outer, uint64_t inner) {
  for (uint32_t i = 0; i < request.key_count; ++i)
    if (key_value(request.outer_keys[i], outer) != key_value(request.inner_keys[i], inner))
      return false;
  return true;
}

template <typename T>
T* at(void* workspace, uint64_t offset) {
  return reinterpret_cast<T*>(static_cast<uint8_t*>(workspace) + offset);
}

template <typename Event>
void completed(Event event, uint64_t* launches) {
  event.wait_and_throw();
  ++*launches;
  pgaccel_record_gpu_exec();
}

pgaccel_status execute(const pgaccel_join_rows_request& request, const Layout& layout,
                       sycl::queue& queue, void* workspace, pgaccel_join_row_pair* output,
                       uint64_t capacity, pgaccel_join_rows_result* result) {
  pgaccel_join_rows_result produced{};
  if (request.outer_rows == 0) {
    *result = produced;
    return PGACCEL_OK;
  }
  auto* heads = at<int32_t>(workspace, layout.heads);
  auto* next = at<int32_t>(workspace, layout.next);
  auto* counts = at<uint64_t>(workspace, layout.counts);
  const uint64_t buckets = layout.capacity;
  // Stage bounded metadata rather than flattening the pointer-rich request
  // into Metal arguments. Optional NULL pointers can change reflection for
  // by-value captures and abort newArgumentEncoderWithBufferIndex. The layout
  // query charges these bytes; no input rows are staged or copied to the host.
  auto* device_request = at<pgaccel_join_rows_request>(workspace, layout.metadata);
  queue.memcpy(device_request, &request, sizeof(request)).wait_and_throw();
  completed(queue.fill(heads, kEmpty, buckets), &produced.kernel_launches);
  if (request.inner_rows != 0) {
    completed(queue.parallel_for(sycl::range<1>(request.inner_rows), [=](sycl::id<1> id) {
      const auto& device = *device_request;
      const uint64_t row = id[0];
      uint64_t hash = 0;
      if (!key_hash(device.inner_keys, device.key_count, row, &hash))
        return;
      sycl::atomic_ref<int32_t, sycl::memory_order::relaxed, sycl::memory_scope::device,
                       sycl::access::address_space::global_space> head(heads[hash & (buckets - 1)]);
      // Nobody follows next pointers until this entire build kernel completes.
      // Distinct-key hash collisions and duplicates share the same safe chain.
      next[row] = head.exchange(static_cast<int32_t>(row));
    }), &produced.kernel_launches);
  }
  completed(queue.parallel_for(sycl::range<1>(request.outer_rows), [=](sycl::id<1> id) {
    const auto& device = *device_request;
    const uint64_t row = id[0];
    uint64_t matches = 0, hash = 0;
    if (key_hash(device.outer_keys, device.key_count, row, &hash)) {
      for (int32_t inner = heads[hash & (buckets - 1)]; inner != kEmpty; inner = next[inner]) {
        if (equal_keys(device, row, static_cast<uint64_t>(inner))) {
          ++matches;
          if (device.kind == PGACCEL_JOIN_ROWS_SEMI || device.kind == PGACCEL_JOIN_ROWS_ANTI)
            break;
        }
      }
    }
    if (device.kind == PGACCEL_JOIN_ROWS_LEFT && matches == 0)
      matches = 1;
    else if (device.kind == PGACCEL_JOIN_ROWS_SEMI)
      matches = matches != 0 ? 1 : 0;
    else if (device.kind == PGACCEL_JOIN_ROWS_ANTI)
      matches = matches == 0 ? 1 : 0;
    counts[row] = matches;
  }), &produced.kernel_launches);

  // Block-local scans plus recursively scanned block sums. No whole-input
  // serial scan or row-proportional host copyback. INT32_MAX per input proves
  // every prefix and the total fit in uint64, including duplicate products.
  for (size_t level_index = 0; level_index < layout.level_count; ++level_index) {
    const auto level = layout.levels[level_index];
    const auto* input = level_index == 0 ? counts : at<uint64_t>(workspace, layout.levels[level_index - 1].sums);
    auto* offsets = at<uint64_t>(workspace, level.offsets);
    auto* sums = at<uint64_t>(workspace, level.sums);
    completed(queue.parallel_for(sycl::range<1>((level.count + kBlock - 1) / kBlock), [=](sycl::id<1> id) {
      const uint64_t begin = id[0] * kBlock;
      uint64_t sum = 0;
      const uint64_t end = sycl::min(begin + kBlock, level.count);
      for (uint64_t row = begin; row < end; ++row) {
        offsets[row] = sum;
        sum += input[row];
      }
      sums[id[0]] = sum;
    }), &produced.kernel_launches);
  }
  for (size_t level_index = layout.level_count - 1; level_index != 0; --level_index) {
    const auto lower = layout.levels[level_index - 1];
    auto* child = at<uint64_t>(workspace, lower.offsets);
    const auto* parent = at<uint64_t>(workspace, layout.levels[level_index].offsets);
    completed(queue.parallel_for(sycl::range<1>(lower.count), [=](sycl::id<1> id) {
      child[id[0]] += parent[id[0] / kBlock];
    }), &produced.kernel_launches);
  }
  const auto* total = at<uint64_t>(workspace, layout.levels[layout.level_count - 1].sums);
  queue.memcpy(&produced.required_rows, total, sizeof(produced.required_rows)).wait_and_throw();
  if (produced.required_rows > capacity) {
    *result = produced;
    return PGACCEL_OOM;
  }
  if (produced.required_rows != 0) {
    const auto* offsets = at<uint64_t>(workspace, layout.levels[0].offsets);
    completed(queue.parallel_for(sycl::range<1>(request.outer_rows), [=](sycl::id<1> id) {
      const auto& device = *device_request;
      const uint64_t row = id[0];
      if (counts[row] == 0)
        return;
      uint64_t position = offsets[row];
      if (device.kind == PGACCEL_JOIN_ROWS_SEMI || device.kind == PGACCEL_JOIN_ROWS_ANTI) {
        output[position] = {row, PGACCEL_JOIN_ROWS_NULL_ROW};
        return;
      }
      uint64_t hash = 0;
      if (key_hash(device.outer_keys, device.key_count, row, &hash)) {
        for (int32_t inner = heads[hash & (buckets - 1)]; inner != kEmpty; inner = next[inner]) {
          if (equal_keys(device, row, static_cast<uint64_t>(inner)))
            output[position++] = {row, static_cast<uint64_t>(inner)};
        }
      }
      if (position == offsets[row] && device.kind == PGACCEL_JOIN_ROWS_LEFT)
        output[position] = {row, PGACCEL_JOIN_ROWS_NULL_ROW};
    }), &produced.kernel_launches);
  }
  produced.emitted_rows = produced.required_rows;
  *result = produced;
  return PGACCEL_OK;
}
} // namespace


extern "C" pgaccel_status pgaccel_join_rows_execute(
    const pgaccel_join_rows_request* request, void* workspace, uint64_t workspace_bytes,
    pgaccel_join_row_pair* output, uint64_t output_capacity,
    pgaccel_join_rows_result* result) try {
  if (request == nullptr || result == nullptr || !aligned(request, alignof(pgaccel_join_rows_request)))
    return PGACCEL_INVALID_ARGUMENT;
  const auto snapshot = *request;
  Layout layout;
  const auto status = layout_for(snapshot, &layout);
  if (status != PGACCEL_OK)
    return status;
  if (workspace_bytes < layout.bytes || !valid_aliases(snapshot, request, workspace, workspace_bytes,
                                                      output, output_capacity, result, sizeof(*result)))
    return PGACCEL_INVALID_ARGUMENT;
  auto& queue = pgaccel_require_queue();
  if (!accessible(queue, workspace) || !accessible(queue, output))
    return PGACCEL_INVALID_ARGUMENT;
  for (size_t key = 0; key < snapshot.key_count; ++key)
    for (const auto* input : {&snapshot.outer_keys[key], &snapshot.inner_keys[key]})
      if (!accessible(queue, input->values) || !accessible(queue, input->nulls))
        return PGACCEL_INVALID_ARGUMENT;
  return execute(snapshot, layout, queue, workspace, output, output_capacity, result);
} catch (const pgaccel_no_device_error&) {
  return PGACCEL_ERROR_NO_DEVICE;
} catch (const std::bad_alloc&) {
  return PGACCEL_OOM;
} catch (const std::exception& error) {
  return pgaccel_kernel_failure("pgaccel_join_rows_execute", &error);
} catch (...) {
  return pgaccel_kernel_failure("pgaccel_join_rows_execute", nullptr);
}

namespace {

struct ControlledRun {
  const pgaccel_join_control& control;
  pgaccel_join_controlled_result* result;
  pgaccel_join_controlled_result produced{};

  bool poll() {
    if (control.cancel_check != nullptr) {
      ++produced.cancel_checks;
      if (control.cancel_check(control.context) != 0) {
        produced.cancelled = 1;
        // Partially emitted device rows never become successful output.
        produced.rows.emitted_rows = 0;
        *result = produced;
        return false;
      }
    }
    return true;
  }

  template <typename Event> bool launch(Event event) {
    completed(event, &produced.rows.kernel_launches);
    return poll();
  }

  template <typename Function> bool ranges(uint64_t count, Function dispatch) {
    for (uint64_t first = 0; first < count;) {
      const uint64_t size = std::min(control.work_items, count - first);
      if (!dispatch(first, size))
        return false;
      first += size;
    }
    return true;
  }
};

pgaccel_status execute_controlled(const pgaccel_join_rows_request& request,
                                  const pgaccel_join_control& control, const Layout& layout,
                                  sycl::queue& queue, void* workspace,
                                  pgaccel_join_row_pair* output, uint64_t capacity,
                                  pgaccel_join_controlled_result* result, bool count_only) {
  ControlledRun run{control, result};
  if (!run.poll())
    return PGACCEL_TIMEOUT;
  if (request.outer_rows == 0) {
    *result = run.produced;
    return PGACCEL_OK;
  }
  auto* device_request = at<pgaccel_join_rows_request>(workspace, layout.metadata);
  auto* heads = at<int32_t>(workspace, layout.heads);
  auto* next = at<int32_t>(workspace, layout.next);
  auto* counts = at<uint64_t>(workspace, layout.counts);
  auto* cursors = at<int32_t>(workspace, layout.cursors);
  auto* positions = at<uint64_t>(workspace, layout.positions);
  auto* pending = at<uint32_t>(workspace, layout.pending);
  const uint64_t buckets = layout.capacity;
  const uint64_t steps = control.chain_steps;
  const uint64_t max_rounds = std::max(UINT64_C(1), (request.inner_rows + steps - 1) / steps);
  queue.memcpy(device_request, &request, sizeof(request)).wait_and_throw();
  if (!run.ranges(buckets, [&](uint64_t first, uint64_t size) {
        return run.launch(queue.fill(heads + first, kEmpty, size));
      }))
    return PGACCEL_TIMEOUT;
  if (!run.ranges(request.inner_rows, [&](uint64_t first, uint64_t size) {
        return run.launch(queue.parallel_for(sycl::range<1>(size), [=](sycl::id<1> id) {
          const auto& device = *device_request;
          const uint64_t row = first + id[0];
          uint64_t hash = 0;
          if (!key_hash(device.inner_keys, device.key_count, row, &hash))
            return;
          sycl::atomic_ref<int32_t, sycl::memory_order::relaxed, sycl::memory_scope::device,
                           sycl::access::address_space::global_space> head(heads[hash & (buckets - 1)]);
          next[row] = head.exchange(static_cast<int32_t>(row));
        }));
      }))
    return PGACCEL_TIMEOUT;

  // Each row resumes its own hash-chain cursor, not a host-side row loop.
  // Finished rows are inert in later rounds. Only one progress word crosses
  // back; duplicate counts and all cursors stay in the charged workspace.
  bool converged = true;
  if (!run.ranges(request.outer_rows, [&](uint64_t first, uint64_t size) {
        for (uint64_t round = 0; round < max_rounds; ++round) {
          if (!run.launch(queue.fill(pending, uint32_t{0}, 1)))
            return false;
          const bool initialize = round == 0;
          if (!run.launch(queue.parallel_for(sycl::range<1>(size), [=](sycl::id<1> id) {
                const auto& device = *device_request;
                const uint64_t row = first + id[0];
                int32_t inner;
                uint64_t matches;
                if (initialize) {
                  uint64_t hash = 0;
                  inner = key_hash(device.outer_keys, device.key_count, row, &hash)
                              ? heads[hash & (buckets - 1)] : kEmpty;
                  matches = 0;
                } else {
                  inner = cursors[row];
                  if (inner == kEmpty)
                    return;
                  matches = counts[row];
                }
                for (uint64_t step = 0; step < steps && inner != kEmpty; ++step) {
                  if (equal_keys(device, row, static_cast<uint64_t>(inner))) {
                    ++matches;
                    if (device.kind == PGACCEL_JOIN_ROWS_SEMI || device.kind == PGACCEL_JOIN_ROWS_ANTI) {
                      inner = kEmpty;
                      break;
                    }
                  }
                  inner = next[inner];
                }
                cursors[row] = inner;
                if (inner != kEmpty) {
                  sycl::atomic_ref<uint32_t, sycl::memory_order::relaxed, sycl::memory_scope::device,
                                   sycl::access::address_space::global_space>(*pending).store(1);
                } else if (device.kind == PGACCEL_JOIN_ROWS_LEFT && matches == 0) {
                  matches = 1;
                } else if (device.kind == PGACCEL_JOIN_ROWS_SEMI) {
                  matches = matches != 0 ? 1 : 0;
                } else if (device.kind == PGACCEL_JOIN_ROWS_ANTI) {
                  matches = matches == 0 ? 1 : 0;
                }
                counts[row] = matches;
              })))
            return false;
          uint32_t more = 0;
          queue.memcpy(&more, pending, sizeof(more)).wait_and_throw();
          if (more == 0)
            return true;
        }
        converged = false;
        return true;
      }))
    return PGACCEL_TIMEOUT;
  if (!converged)
    return PGACCEL_ERROR;

  for (size_t index = 0; index < layout.level_count; ++index) {
    const auto level = layout.levels[index];
    // Only the row bound is needed by the scan kernel. Snapshot that scalar
    // into the host callback instead of capturing the host layout by reference
    // and then copying the complete level into the nested device closure.
    const uint64_t level_rows = level.count;
    const auto* input = index == 0 ? counts : at<uint64_t>(workspace, layout.levels[index - 1].sums);
    auto* offsets = at<uint64_t>(workspace, level.offsets);
    auto* sums = at<uint64_t>(workspace, level.sums);
    if (!run.ranges((level_rows + kBlock - 1) / kBlock, [&, level_rows](uint64_t first, uint64_t size) {
          return run.launch(queue.parallel_for(sycl::range<1>(size), [=](sycl::id<1> id) {
            const uint64_t block = first + id[0];
            const uint64_t begin = block * kBlock;
            const uint64_t end = sycl::min(begin + kBlock, level_rows);
            uint64_t sum = 0;
            for (uint64_t row = begin; row < end; ++row) {
              offsets[row] = sum;
              sum += input[row];
            }
            sums[block] = sum;
          }));
        }))
      return PGACCEL_TIMEOUT;
  }
  // The upward scan already produced the exact scalar cardinality. Count-only
  // consumers need neither global per-row offsets nor a second chain traversal.
  const auto* total = at<uint64_t>(workspace, layout.levels[layout.level_count - 1].sums);
  queue.memcpy(&run.produced.rows.required_rows, total, sizeof(uint64_t)).wait_and_throw();
  if (count_only) {
    *result = run.produced;
    return PGACCEL_OK;
  }
  for (size_t index = layout.level_count - 1; index != 0; --index) {
    const auto level = layout.levels[index - 1];
    auto* child = at<uint64_t>(workspace, level.offsets);
    const auto* parent = at<uint64_t>(workspace, layout.levels[index].offsets);
    if (!run.ranges(level.count, [&](uint64_t first, uint64_t size) {
          return run.launch(queue.parallel_for(sycl::range<1>(size), [=](sycl::id<1> id) {
            const uint64_t row = first + id[0];
            child[row] += parent[row / kBlock];
          }));
        }))
      return PGACCEL_TIMEOUT;
  }
  if (run.produced.rows.required_rows > capacity) {
    *result = run.produced;
    return PGACCEL_OOM;
  }
  if (run.produced.rows.required_rows != 0) {
    const auto* offsets = at<uint64_t>(workspace, layout.levels[0].offsets);
    if (!run.ranges(request.outer_rows, [&](uint64_t first, uint64_t size) {
          for (uint64_t round = 0; round < max_rounds; ++round) {
            if (!run.launch(queue.fill(pending, uint32_t{0}, 1)))
              return false;
            const bool initialize = round == 0;
            if (!run.launch(queue.parallel_for(sycl::range<1>(size), [=](sycl::id<1> id) {
                  const auto& device = *device_request;
                  const uint64_t row = first + id[0];
                  int32_t inner;
                  uint64_t position;
                  if (initialize) {
                    cursors[row] = kEmpty;
                    if (counts[row] == 0)
                      return;
                    position = offsets[row];
                    if (device.kind == PGACCEL_JOIN_ROWS_SEMI || device.kind == PGACCEL_JOIN_ROWS_ANTI) {
                      output[position] = {row, PGACCEL_JOIN_ROWS_NULL_ROW};
                      return;
                    }
                    uint64_t hash = 0;
                    inner = key_hash(device.outer_keys, device.key_count, row, &hash)
                                ? heads[hash & (buckets - 1)] : kEmpty;
                  } else {
                    inner = cursors[row];
                    if (inner == kEmpty)
                      return;
                    position = positions[row];
                  }
                  for (uint64_t step = 0; step < steps && inner != kEmpty; ++step) {
                    if (equal_keys(device, row, static_cast<uint64_t>(inner)))
                      output[position++] = {row, static_cast<uint64_t>(inner)};
                    inner = next[inner];
                  }
                  cursors[row] = inner;
                  positions[row] = position;
                  if (inner != kEmpty) {
                    sycl::atomic_ref<uint32_t, sycl::memory_order::relaxed, sycl::memory_scope::device,
                                     sycl::access::address_space::global_space>(*pending).store(1);
                  } else if (position == offsets[row] && device.kind == PGACCEL_JOIN_ROWS_LEFT) {
                    output[position] = {row, PGACCEL_JOIN_ROWS_NULL_ROW};
                  }
                })))
              return false;
            uint32_t more = 0;
            queue.memcpy(&more, pending, sizeof(more)).wait_and_throw();
            if (more == 0)
              return true;
          }
          converged = false;
          return true;
        }))
      return PGACCEL_TIMEOUT;
  }
  if (!converged)
    return PGACCEL_ERROR;
  run.produced.rows.emitted_rows = run.produced.rows.required_rows;
  *result = run.produced;
  return PGACCEL_OK;
}
} // namespace


static pgaccel_status dispatch_controlled(
    const pgaccel_join_rows_request* request, const pgaccel_join_control* control,
    void* workspace, uint64_t workspace_bytes, pgaccel_join_row_pair* output,
    uint64_t output_capacity, pgaccel_join_controlled_result* result, bool count_only) try {
  if (request == nullptr || control == nullptr || result == nullptr ||
      !aligned(request, alignof(pgaccel_join_rows_request)) ||
      !aligned(control, alignof(pgaccel_join_control)))
    return PGACCEL_INVALID_ARGUMENT;
  const auto snapshot = *request;
  const auto options = *control;
  Layout layout;
  const auto status = controlled_layout(snapshot, options, &layout);
  if (status != PGACCEL_OK)
    return status;
  if (workspace_bytes < layout.bytes ||
      !valid_aliases(snapshot, request, workspace, workspace_bytes, output, output_capacity, result, sizeof(*result)))
    return PGACCEL_INVALID_ARGUMENT;
  Span metadata, scratch, pairs, destination;
  if (!span(control, sizeof(*control), &metadata) || !span(workspace, workspace_bytes, &scratch) ||
      !span(output, output_capacity * sizeof(*output), &pairs) || !span(result, sizeof(*result), &destination) ||
      overlaps(metadata, scratch) || overlaps(metadata, pairs) || overlaps(metadata, destination))
    return PGACCEL_INVALID_ARGUMENT;
  auto& queue = pgaccel_require_queue();
  if (!accessible(queue, workspace) || !accessible(queue, output))
    return PGACCEL_INVALID_ARGUMENT;
  for (size_t key = 0; key < snapshot.key_count; ++key)
    for (const auto* input : {&snapshot.outer_keys[key], &snapshot.inner_keys[key]})
      if (!accessible(queue, input->values) || !accessible(queue, input->nulls))
        return PGACCEL_INVALID_ARGUMENT;
  return execute_controlled(snapshot, options, layout, queue, workspace, output, output_capacity, result, count_only);
} catch (const pgaccel_no_device_error&) {
  return PGACCEL_ERROR_NO_DEVICE;
} catch (const std::bad_alloc&) {
  return PGACCEL_OOM;
} catch (const std::exception& error) {
  return pgaccel_kernel_failure("pgaccel_join_rows_execute_controlled", &error);
} catch (...) {
  return pgaccel_kernel_failure("pgaccel_join_rows_execute_controlled", nullptr);
}

extern "C" pgaccel_status pgaccel_join_rows_execute_controlled(
    const pgaccel_join_rows_request* request, const pgaccel_join_control* control,
    void* workspace, uint64_t workspace_bytes, pgaccel_join_row_pair* output,
    uint64_t output_capacity, pgaccel_join_controlled_result* result) {
  return dispatch_controlled(request, control, workspace, workspace_bytes, output, output_capacity, result, false);
}

extern "C" pgaccel_status pgaccel_join_rows_count_controlled(
    const pgaccel_join_rows_request* request, const pgaccel_join_control* control,
    void* workspace, uint64_t workspace_bytes, pgaccel_join_controlled_result* result) {
  return dispatch_controlled(request, control, workspace, workspace_bytes, nullptr, 0, result, true);
}

namespace {
bool project_contract(const pgaccel_join_project_request& request, const void* metadata,
                      void* workspace, uint64_t workspace_bytes, void* result,
                      uint64_t result_bytes, const pgaccel_join_control* control = nullptr) {
  if (request.column_count == 0 || request.column_count > PGACCEL_JOIN_PROJECT_MAX_COLUMNS ||
      request.reserved != 0 || request.outer_rows > INT32_MAX || request.inner_rows > INT32_MAX ||
      request.row_count > SIZE_MAX / sizeof(pgaccel_join_row_pair) ||
      !aligned(request.pairs, alignof(pgaccel_join_row_pair)) ||
      workspace_bytes < PGACCEL_JOIN_PROJECT_WORKSPACE_BYTES ||
      !aligned(workspace, 8) || !aligned(result, alignof(pgaccel_join_project_result)))
    return false;
  std::array<Span, 3 + 2 * PGACCEL_JOIN_PROJECT_MAX_COLUMNS> reads{}, writes{};
  size_t read_count = 3, write_count = 2;
  if (!span(metadata, sizeof(request), &reads[0]) ||
      !span(request.pairs, request.pair_bytes, &reads[1]) ||
      !span(control, control == nullptr ? 0 : sizeof(*control), &reads[2]) ||
      !span(workspace, workspace_bytes, &writes[0]) ||
      !span(result, result_bytes, &writes[1]) ||
      request.pair_bytes < request.row_count * sizeof(pgaccel_join_row_pair) ||
      (request.row_count == 0 && request.pair_bytes != 0))
    return false;
  const pgaccel_join_project_column zero{};
  for (size_t index = 0; index < PGACCEL_JOIN_PROJECT_MAX_COLUMNS; ++index) {
    const auto& column = request.columns[index];
    if (index >= request.column_count) {
      if (std::memcmp(&column, &zero, sizeof(zero)) != 0)
        return false;
      continue;
    }
    const uint64_t source_rows = column.side == PGACCEL_JOIN_PROJECT_OUTER ? request.outer_rows : request.inner_rows;
    if (column.side > PGACCEL_JOIN_PROJECT_INNER ||
        (column.width != 1 && column.width != 4 && column.width != 8) ||
        !aligned(column.values, column.width) || !aligned(column.output_values, column.width) ||
        !span(column.values, column.value_bytes, &reads[read_count++]) ||
        !span(column.nulls, column.null_bytes, &reads[read_count++]) ||
        !span(column.output_values, column.output_value_bytes, &writes[write_count++]) ||
        !span(column.output_nulls, column.output_null_bytes, &writes[write_count++]) ||
        column.value_bytes < source_rows * column.width ||
        (column.nulls != nullptr && column.null_bytes < source_rows) ||
        column.output_value_bytes < request.row_count * column.width ||
        column.output_null_bytes < request.row_count ||
        (source_rows == 0 && (column.value_bytes != 0 || column.null_bytes != 0)) ||
        (request.row_count == 0 && (column.output_value_bytes != 0 || column.output_null_bytes != 0)))
      return false;
  }
  for (size_t left = 0; left < write_count; ++left) {
    for (size_t right = left + 1; right < write_count; ++right)
      if (overlaps(writes[left], writes[right]))
        return false;
    for (size_t input = 0; input < read_count; ++input)
      if (overlaps(writes[left], reads[input]))
        return false;
  }
  return true;
}

template <typename T>
void project_value(const pgaccel_join_project_column& column, uint64_t source, uint64_t target, bool is_null) {
  static_cast<T*>(column.output_values)[target] = is_null ? T{0} : static_cast<const T*>(column.values)[source];
}
} // namespace

extern "C" pgaccel_status pgaccel_join_rows_project(
    const pgaccel_join_project_request* request, void* workspace, uint64_t workspace_bytes,
    pgaccel_join_project_result* result) try {
  if (request == nullptr || !aligned(request, alignof(pgaccel_join_project_request)))
    return PGACCEL_INVALID_ARGUMENT;
  const auto snapshot = *request;
  if (snapshot.version != PGACCEL_JOIN_PROJECT_VERSION)
    return PGACCEL_UNSUPPORTED;
  if (!project_contract(snapshot, request, workspace, workspace_bytes, result, sizeof(*result)))
    return PGACCEL_INVALID_ARGUMENT;
  auto& queue = pgaccel_require_queue();
  if (!accessible(queue, snapshot.pairs) || !accessible(queue, workspace))
    return PGACCEL_INVALID_ARGUMENT;
  for (size_t index = 0; index < snapshot.column_count; ++index) {
    const auto& column = snapshot.columns[index];
    if (!accessible(queue, column.values) || !accessible(queue, column.nulls) ||
        !accessible(queue, column.output_values) || !accessible(queue, column.output_nulls))
      return PGACCEL_INVALID_ARGUMENT;
  }
  pgaccel_join_project_result produced{};
  if (snapshot.row_count == 0) {
    *result = produced;
    return PGACCEL_OK;
  }
  auto* device_error = static_cast<uint32_t*>(workspace);
  auto* columns = at<pgaccel_join_project_column>(workspace, 8);
  const auto* pairs = snapshot.pairs;
  const uint64_t outer_rows = snapshot.outer_rows, inner_rows = snapshot.inner_rows;
  const uint32_t column_count = snapshot.column_count;
  // Keep the pointer-rich host descriptor out of the kernel capture ABI.
  // This fixed metadata copy is included in caller-owned workspace bytes.
  queue.memcpy(columns, snapshot.columns, sizeof(snapshot.columns)).wait_and_throw();
  completed(queue.fill(device_error, uint32_t{0}, 1), &produced.kernel_launches);
  completed(queue.parallel_for(sycl::range<1>(snapshot.row_count), [=](sycl::id<1> id) {
    const auto pair = pairs[id[0]];
    if (pair.outer_row >= outer_rows ||
        (pair.inner_row != PGACCEL_JOIN_ROWS_NULL_ROW && pair.inner_row >= inner_rows)) {
      sycl::atomic_ref<uint32_t, sycl::memory_order::relaxed, sycl::memory_scope::device,
                       sycl::access::address_space::global_space> error(*device_error);
      error.store(1);
    }
  }), &produced.kernel_launches);
  uint32_t invalid = 0;
  queue.memcpy(&invalid, device_error, sizeof(invalid)).wait_and_throw();
  if (invalid != 0)
    return PGACCEL_INVALID_ARGUMENT;
  completed(queue.parallel_for(sycl::range<1>(snapshot.row_count), [=](sycl::id<1> id) {
    const uint64_t row = id[0];
    const auto pair = pairs[row];
    for (uint32_t index = 0; index < column_count; ++index) {
      const auto column = columns[index];
      const uint64_t source = column.side == PGACCEL_JOIN_PROJECT_OUTER ? pair.outer_row : pair.inner_row;
      const bool is_null = source == PGACCEL_JOIN_ROWS_NULL_ROW ||
                           (column.nulls != nullptr && column.nulls[source] != 0);
      column.output_nulls[row] = is_null ? 1 : 0;
      if (column.width == 1)
        project_value<uint8_t>(column, source, row, is_null);
      else if (column.width == 4)
        project_value<uint32_t>(column, source, row, is_null);
      else
        project_value<uint64_t>(column, source, row, is_null);
    }
  }), &produced.kernel_launches);
  produced.emitted_rows = snapshot.row_count;
  *result = produced;
  return PGACCEL_OK;
} catch (const pgaccel_no_device_error&) {
  return PGACCEL_ERROR_NO_DEVICE;
} catch (const std::bad_alloc&) {
  return PGACCEL_OOM;
} catch (const std::exception& error) {
  return pgaccel_kernel_failure("pgaccel_join_rows_project", &error);
} catch (...) {
  return pgaccel_kernel_failure("pgaccel_join_rows_project", nullptr);
}

extern "C" pgaccel_status pgaccel_join_rows_project_controlled(
    const pgaccel_join_project_request* request, const pgaccel_join_control* control,
    void* workspace, uint64_t workspace_bytes, pgaccel_join_controlled_result* result) try {
  if (request == nullptr || control == nullptr ||
      !aligned(request, alignof(pgaccel_join_project_request)) ||
      !aligned(control, alignof(pgaccel_join_control)))
    return PGACCEL_INVALID_ARGUMENT;
  const auto snapshot = *request;
  const auto options = *control;
  if (snapshot.version != PGACCEL_JOIN_PROJECT_VERSION)
    return PGACCEL_UNSUPPORTED;
  const auto status = control_contract(options);
  if (status != PGACCEL_OK)
    return status;
  if (!project_contract(snapshot, request, workspace, workspace_bytes, result, sizeof(*result), control))
    return PGACCEL_INVALID_ARGUMENT;
  auto& queue = pgaccel_require_queue();
  if (!accessible(queue, snapshot.pairs) || !accessible(queue, workspace))
    return PGACCEL_INVALID_ARGUMENT;
  for (size_t index = 0; index < snapshot.column_count; ++index) {
    const auto& column = snapshot.columns[index];
    if (!accessible(queue, column.values) || !accessible(queue, column.nulls) ||
        !accessible(queue, column.output_values) || !accessible(queue, column.output_nulls))
      return PGACCEL_INVALID_ARGUMENT;
  }
  ControlledRun run{options, result};
  if (!run.poll())
    return PGACCEL_TIMEOUT;
  if (snapshot.row_count == 0) {
    *result = run.produced;
    return PGACCEL_OK;
  }
  auto* device_error = static_cast<uint32_t*>(workspace);
  auto* columns = at<pgaccel_join_project_column>(workspace, 8);
  const auto* pairs = snapshot.pairs;
  const uint64_t outer_rows = snapshot.outer_rows, inner_rows = snapshot.inner_rows;
  const uint32_t column_count = snapshot.column_count;
  queue.memcpy(columns, snapshot.columns, sizeof(snapshot.columns)).wait_and_throw();
  if (!run.launch(queue.fill(device_error, uint32_t{0}, 1)))
    return PGACCEL_TIMEOUT;
  if (!run.ranges(snapshot.row_count, [&](uint64_t first, uint64_t size) {
        return run.launch(queue.parallel_for(sycl::range<1>(size), [=](sycl::id<1> id) {
          const auto pair = pairs[first + id[0]];
          if (pair.outer_row >= outer_rows ||
              (pair.inner_row != PGACCEL_JOIN_ROWS_NULL_ROW && pair.inner_row >= inner_rows)) {
            sycl::atomic_ref<uint32_t, sycl::memory_order::relaxed, sycl::memory_scope::device,
                             sycl::access::address_space::global_space>(*device_error).store(1);
          }
        }));
      }))
    return PGACCEL_TIMEOUT;
  uint32_t invalid = 0;
  queue.memcpy(&invalid, device_error, sizeof(invalid)).wait_and_throw();
  if (invalid != 0)
    return PGACCEL_INVALID_ARGUMENT;
  run.produced.rows.required_rows = snapshot.row_count;
  if (!run.ranges(snapshot.row_count, [&](uint64_t first, uint64_t size) {
        return run.launch(queue.parallel_for(sycl::range<1>(size), [=](sycl::id<1> id) {
          const uint64_t row = first + id[0];
          const auto pair = pairs[row];
          for (uint32_t index = 0; index < column_count; ++index) {
            const auto column = columns[index];
            const uint64_t source = column.side == PGACCEL_JOIN_PROJECT_OUTER ? pair.outer_row : pair.inner_row;
            const bool is_null = source == PGACCEL_JOIN_ROWS_NULL_ROW ||
                                (column.nulls != nullptr && column.nulls[source] != 0);
            column.output_nulls[row] = is_null ? 1 : 0;
            if (column.width == 1)
              project_value<uint8_t>(column, source, row, is_null);
            else if (column.width == 4)
              project_value<uint32_t>(column, source, row, is_null);
            else
              project_value<uint64_t>(column, source, row, is_null);
          }
        }));
      }))
    return PGACCEL_TIMEOUT;
  run.produced.rows.emitted_rows = snapshot.row_count;
  *result = run.produced;
  return PGACCEL_OK;
} catch (const pgaccel_no_device_error&) {
  return PGACCEL_ERROR_NO_DEVICE;
} catch (const std::bad_alloc&) {
  return PGACCEL_OOM;
} catch (const std::exception& error) {
  return pgaccel_kernel_failure("pgaccel_join_rows_project_controlled", &error);
} catch (...) {
  return pgaccel_kernel_failure("pgaccel_join_rows_project_controlled", nullptr);
}

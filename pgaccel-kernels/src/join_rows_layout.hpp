// Internal metadata-only layout helpers, shared by query and execution validation.
// No SYCL types, device discovery, row-value reads, allocation or callbacks belong here.
#ifndef PGACCEL_JOIN_ROWS_LAYOUT_HPP
#define PGACCEL_JOIN_ROWS_LAYOUT_HPP

#include <stddef.h>
#include <stdint.h>
#include <string.h>

#include "pgaccel_join_rows.h"

namespace pgaccel_join_detail {
// Internal linkage prevents a separately compiled SYCL translation unit from
// replacing a proved metadata helper through weak/COMDAT symbol coalescing.
constexpr uint64_t kBlock = 256;
constexpr size_t kMaxLevels = 4; // ceil(INT32_MAX / 256^3) <= 256

struct Span {
  uintptr_t begin = 0;
  uintptr_t end = 0;
};

static inline bool span(const void* pointer, uint64_t bytes, Span* out) {
  if (bytes == 0) {
    *out = {};
    return pointer == nullptr;
  }
  if (pointer == nullptr || bytes > SIZE_MAX)
    return false;
  const uintptr_t begin = reinterpret_cast<uintptr_t>(pointer);
  if (bytes > UINTPTR_MAX - begin)
    return false;
  *out = {begin, begin + static_cast<uintptr_t>(bytes)};
  return true;
}

static inline bool overlaps(Span a, Span b) {
  return a.begin < b.end && b.begin < a.end;
}

static inline bool aligned(const void* pointer, size_t alignment) {
  return reinterpret_cast<uintptr_t>(pointer) % alignment == 0;
}

struct Level {
  uint64_t count = 0;
  uint64_t offsets = 0;
  uint64_t sums = 0;
};

struct Layout {
  uint64_t bytes = 0;
  uint64_t capacity = 16;
  uint64_t metadata = 0;
  uint64_t heads = 0;
  uint64_t next = 0;
  uint64_t counts = 0;
  uint64_t cursors = 0;
  uint64_t positions = 0;
  uint64_t pending = 0;
  Level levels[kMaxLevels]{};
  size_t level_count = 0;
};

static inline bool reserve(Layout* layout, uint64_t count, uint64_t width, uint64_t* offset) {
  if (count > (SIZE_MAX - 7) / width || layout->bytes > SIZE_MAX - 7)
    return false;
  const uint64_t bytes = (count * width + 7) & ~UINT64_C(7);
  if (bytes > SIZE_MAX - layout->bytes)
    return false;
  *offset = layout->bytes;
  layout->bytes += bytes;
  return true;
}

static inline bool valid_key(const pgaccel_join_rows_key& key, uint64_t rows) {
  const uint64_t width = key.type == PGACCEL_JOIN_ROWS_INT32 ? 4 : 8;
  Span ignored;
  if ((key.type != PGACCEL_JOIN_ROWS_INT32 && key.type != PGACCEL_JOIN_ROWS_INT64) ||
      key.reserved != 0 || !aligned(key.values, width) ||
      !span(key.values, key.value_bytes, &ignored) ||
      !span(key.nulls, key.null_bytes, &ignored))
    return false;
  if (rows == 0)
    return key.value_bytes == 0 && key.null_bytes == 0;
  return key.values != nullptr && key.value_bytes >= rows * width &&
         (key.nulls == nullptr || key.null_bytes >= rows);
}

static inline pgaccel_status layout_for(const pgaccel_join_rows_request& request, Layout* layout) {
  if (request.version != PGACCEL_JOIN_ROWS_VERSION)
    return PGACCEL_UNSUPPORTED;
  if (request.kind > PGACCEL_JOIN_ROWS_ANTI || request.reserved != 0 ||
      request.key_count == 0 || request.key_count > PGACCEL_JOIN_ROWS_MAX_KEYS)
    return PGACCEL_INVALID_ARGUMENT;
  if (request.outer_rows > INT32_MAX || request.inner_rows > INT32_MAX)
    return PGACCEL_UNSUPPORTED;
  const pgaccel_join_rows_key zero{};
  for (size_t key = 0; key < PGACCEL_JOIN_ROWS_MAX_KEYS; ++key) {
    if (key >= request.key_count) {
      if (::memcmp(&request.outer_keys[key], &zero, sizeof(zero)) != 0 ||
          ::memcmp(&request.inner_keys[key], &zero, sizeof(zero)) != 0)
        return PGACCEL_INVALID_ARGUMENT;
    } else if (!valid_key(request.outer_keys[key], request.outer_rows) ||
               !valid_key(request.inner_keys[key], request.inner_rows) ||
               request.outer_keys[key].type != request.inner_keys[key].type) {
      return PGACCEL_INVALID_ARGUMENT;
    }
  }
  // All host loops have a fixed metadata bound, independent of the row domain.
  // Starting at 16, 28 doublings suffice for the supported INT32_MAX rows.
  for (unsigned step = 0; step < 32; ++step) {
    if (layout->capacity >= request.inner_rows * 2)
      break;
    layout->capacity *= 2;
  }
  if (layout->capacity < request.inner_rows * 2)
    return PGACCEL_UNSUPPORTED;
  if (request.outer_rows != 0 &&
      !reserve(layout, 1, sizeof(pgaccel_join_rows_request), &layout->metadata))
    return PGACCEL_UNSUPPORTED;
  if (!reserve(layout, layout->capacity, 4, &layout->heads) ||
      !reserve(layout, request.inner_rows, 4, &layout->next) ||
      !reserve(layout, request.outer_rows, 8, &layout->counts))
    return PGACCEL_UNSUPPORTED;
  uint64_t count = request.outer_rows;
  for (size_t depth = 0; depth < kMaxLevels && count != 0; ++depth) {
    auto& level = layout->levels[depth];
    layout->level_count = depth + 1;
    level.count = count;
    count = (count + kBlock - 1) / kBlock;
    if (!reserve(layout, level.count, 8, &level.offsets) ||
        !reserve(layout, count, 8, &level.sums))
      return PGACCEL_UNSUPPORTED;
    if (count == 1)
      break;
  }
  if (count > 1)
    return PGACCEL_UNSUPPORTED;
  return PGACCEL_OK;
}

static inline pgaccel_status control_contract(const pgaccel_join_control& control) {
  if (control.version != PGACCEL_JOIN_CONTROL_VERSION)
    return PGACCEL_UNSUPPORTED;
  if (control.reserved != 0 || control.work_items == 0 || control.work_items > INT32_MAX ||
      control.chain_steps == 0 || control.chain_steps > INT32_MAX ||
      (control.cancel_check == nullptr && control.context != nullptr))
    return PGACCEL_INVALID_ARGUMENT;
  return PGACCEL_OK;
}

static inline pgaccel_status controlled_layout(const pgaccel_join_rows_request& request,
                                 const pgaccel_join_control& control, Layout* layout) {
  const auto options = control_contract(control);
  if (options != PGACCEL_OK)
    return options;
  const auto status = layout_for(request, layout);
  if (status != PGACCEL_OK || request.outer_rows == 0)
    return status;
  if (!reserve(layout, request.outer_rows, 4, &layout->cursors) ||
      !reserve(layout, request.outer_rows, 8, &layout->positions) ||
      !reserve(layout, 1, 4, &layout->pending))
    return PGACCEL_UNSUPPORTED;
  return PGACCEL_OK;
}

static inline bool query_key_aliases(const pgaccel_join_rows_key& column, Span output) {
  Span values, nulls;
  return span(column.values, column.value_bytes, &values) &&
         span(column.nulls, column.null_bytes, &nulls) &&
         !overlaps(output, values) && !overlaps(output, nulls);
}

static inline bool controlled_query_aliases(const pgaccel_join_rows_request* request,
                              const pgaccel_join_control* control, uint64_t* bytes) {
  Span out, metadata;
  if (!span(bytes, sizeof(*bytes), &out) ||
      !span(request, sizeof(*request), &metadata) || overlaps(out, metadata) ||
      !span(control, sizeof(*control), &metadata) || overlaps(out, metadata))
    return false;
  for (size_t key = 0; key < PGACCEL_JOIN_ROWS_MAX_KEYS; ++key) {
    if (key >= request->key_count)
      break;
    if (!query_key_aliases(request->outer_keys[key], out) ||
        !query_key_aliases(request->inner_keys[key], out))
      return false;
  }
  return true;
}

} // namespace pgaccel_join_detail

#endif

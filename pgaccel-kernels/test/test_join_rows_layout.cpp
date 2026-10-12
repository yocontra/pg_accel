// Builds the real workspace-query exports without any GPU library. Row buffers
// are live virtual allocations with PROT_NONE: any payload read/write faults.
#include <array>
#include <cstdint>
#include <cstdio>
#include <cstdlib>
#include <cstring>
#include <sys/mman.h>
#include <unistd.h>

#include "pgaccel_join_rows.h"

namespace {
unsigned passed = 0;
unsigned failed = 0;
constexpr uint64_t poison = UINT64_C(0x123456789abcdef0);

void check(bool condition, const char* message) {
  if (condition) ++passed;
  else { ++failed; std::fprintf(stderr, "FAIL: %s\n", message); }
}

int forbidden_callback(void*) {
  std::fprintf(stderr, "FAIL: workspace query called the cancellation callback\n");
  std::abort();
}

struct InaccessibleRows {
  void* data = MAP_FAILED;
  size_t size = 0;
  InaccessibleRows() {
    const long page = sysconf(_SC_PAGESIZE);
    if (page <= 0) return;
    const uint64_t needed = uint64_t{INT32_MAX} * 8;
    size = static_cast<size_t>((needed + page - 1) / page * page);
    data = mmap(nullptr, size, PROT_NONE, MAP_PRIVATE | MAP_ANON, -1, 0);
  }
  InaccessibleRows(const InaccessibleRows&) = delete;
  InaccessibleRows& operator=(const InaccessibleRows&) = delete;
  ~InaccessibleRows() {
    if (data != MAP_FAILED) check(munmap(data, size) == 0, "guard allocation released");
  }
};

pgaccel_join_rows_request request_for(const InaccessibleRows& storage, uint64_t outer,
                                     uint64_t inner, uint32_t keys, unsigned null_mask) {
  pgaccel_join_rows_request request{};
  request.version = PGACCEL_JOIN_ROWS_VERSION;
  request.key_count = keys;
  request.outer_rows = outer;
  request.inner_rows = inner;
  for (uint32_t key = 0; key < keys; ++key) {
    const uint32_t type = key % 2 ? PGACCEL_JOIN_ROWS_INT64 : PGACCEL_JOIN_ROWS_INT32;
    const uint64_t width = type == PGACCEL_JOIN_ROWS_INT32 ? 4 : 8;
    request.outer_keys[key] = {
        outer ? storage.data : nullptr,
        outer && (null_mask & 1) ? static_cast<const uint8_t*>(storage.data) : nullptr,
        outer * width, outer && (null_mask & 1) ? outer : 0, type, 0};
    request.inner_keys[key] = {
        inner ? storage.data : nullptr,
        inner && (null_mask & 2) ? static_cast<const uint8_t*>(storage.data) : nullptr,
        inner * width, inner && (null_mask & 2) ? inner : 0, type, 0};
  }
  return request;
}

// Independent byte accounting from the documented storage components. In
// particular, the hash capacity is derived from bit width, not the producer's
// doubling loop; no implementation header or layout helper is included here.
uint64_t expected_bytes(uint64_t outer, uint64_t inner, bool controlled) {
  unsigned exponent = 4;
  for (uint64_t high = (inner ? inner * 2 - 1 : 0) >> 4; high; high >>= 1) ++exponent;
  uint64_t bytes = (UINT64_C(1) << exponent) * 4 + ((inner + 1) / 2) * 8 + outer * 8;
  if (outer) bytes += sizeof(pgaccel_join_rows_request);
  for (uint64_t rows = outer; rows;) {
    const uint64_t blocks = (rows + 255) / 256;
    bytes += (rows + blocks) * 8;
    if (blocks == 1) break;
    rows = blocks;
  }
  if (controlled && outer) bytes += ((outer + 1) / 2) * 8 + outer * 8 + 8;
  return bytes;
}

void valid_layouts(const InaccessibleRows& storage) {
  const std::array<uint64_t, 15> outer_counts{
      0, 1, 15, 16, 17, 255, 256, 257, 65535, 65536, 65537,
      16777215, 16777216, 16777217, INT32_MAX};
  const std::array<uint64_t, 7> inner_counts{0, 1, 7, 8, 9, 65537, INT32_MAX};
  for (uint64_t outer : outer_counts) for (uint64_t inner : inner_counts)
    for (uint32_t keys = 1; keys <= PGACCEL_JOIN_ROWS_MAX_KEYS; ++keys)
      for (unsigned mask = 0; mask < 4; ++mask)
        for (uint32_t kind = PGACCEL_JOIN_ROWS_INNER; kind <= PGACCEL_JOIN_ROWS_ANTI; ++kind) {
          auto request = request_for(storage, outer, inner, keys, mask);
          request.kind = kind;
          const auto original = request;
          uint64_t bytes = poison;
          check(pgaccel_join_rows_workspace_bytes(&request, &bytes) == PGACCEL_OK &&
                    bytes == expected_bytes(outer, inner, false), "exact ordinary layout");
          for (uint64_t limit : {UINT64_C(1), uint64_t{INT32_MAX}}) {
            pgaccel_join_control control{PGACCEL_JOIN_CONTROL_VERSION, 0, limit, limit,
                                         forbidden_callback, storage.data};
            const auto saved = control;
            bytes = poison;
            check(pgaccel_join_rows_controlled_workspace_bytes(&request, &control, &bytes) == PGACCEL_OK &&
                      bytes == expected_bytes(outer, inner, true), "exact controlled layout");
            check(std::memcmp(&control, &saved, sizeof(control)) == 0, "control remains unchanged");
          }
          check(std::memcmp(&request, &original, sizeof(request)) == 0, "request remains unchanged");
        }
}

void invalid_layouts(const InaccessibleRows& storage) {
  const auto base = request_for(storage, 17, 9, 1, 3);
  const pgaccel_join_control valid{PGACCEL_JOIN_CONTROL_VERSION, 0, 1, 1, nullptr, nullptr};
  for (unsigned mutation = 0; mutation < 23; ++mutation) {
    auto request = base;
    pgaccel_status expected = PGACCEL_INVALID_ARGUMENT;
    switch (mutation) {
      case 0: request.version++; expected = PGACCEL_UNSUPPORTED; break;
      case 1: request.kind = PGACCEL_JOIN_ROWS_ANTI + 1; break;
      case 2: request.reserved = 1; break;
      case 3: request.key_count = 0; break;
      case 4: request.key_count = PGACCEL_JOIN_ROWS_MAX_KEYS + 1; break;
      case 5: request.outer_rows = uint64_t{INT32_MAX} + 1; expected = PGACCEL_UNSUPPORTED; break;
      case 6: request.inner_rows = UINT64_MAX; expected = PGACCEL_UNSUPPORTED; break;
      case 7: request.outer_keys[0].reserved = 1; break;
      case 8: request.inner_keys[0].type = 99; break;
      case 9: request.inner_keys[0].type = PGACCEL_JOIN_ROWS_INT64; break;
      case 10: request.outer_keys[0].value_bytes--; break;
      case 11: request.inner_keys[0].null_bytes--; break;
      case 12: request.outer_keys[0].values = nullptr; break;
      case 13: request.outer_keys[0].nulls = nullptr; break;
      case 14: request.inner_keys[0].values = static_cast<const uint8_t*>(storage.data) + 1; break;
      case 15: request.outer_keys[1].type = PGACCEL_JOIN_ROWS_INT32; break;
      case 16: request.inner_keys[3].values = storage.data; break;
      case 17: request.outer_rows = 0; break;
      case 18: request.inner_keys[0].null_bytes = 0; break;
      case 19: request.inner_keys[0].value_bytes = UINT64_MAX; break;
      case 20: request.outer_keys[0].values = reinterpret_cast<const void*>(UINTPTR_MAX - 7); break;
      case 21: request.inner_keys[0].nulls = reinterpret_cast<const uint8_t*>(UINTPTR_MAX - 3); break;
      case 22: request.outer_keys[0].values = nullptr; request.outer_keys[0].value_bytes = 0; break;
    }
    uint64_t bytes = poison;
    check(pgaccel_join_rows_workspace_bytes(&request, &bytes) == expected && bytes == poison,
          "invalid ordinary descriptor preserves output");
    check(pgaccel_join_rows_controlled_workspace_bytes(&request, &valid, &bytes) == expected && bytes == poison,
          "invalid controlled descriptor preserves output");
  }
  for (unsigned mutation = 0; mutation < 7; ++mutation) {
    auto control = valid;
    switch (mutation) {
      case 0: control.version++; break;
      case 1: control.reserved = 1; break;
      case 2: control.work_items = 0; break;
      case 3: control.work_items = uint64_t{INT32_MAX} + 1; break;
      case 4: control.chain_steps = 0; break;
      case 5: control.chain_steps = UINT64_MAX; break;
      case 6: control.context = storage.data; break;
    }
    uint64_t bytes = poison;
    check(pgaccel_join_rows_controlled_workspace_bytes(&base, &control, &bytes) ==
              (mutation == 0 ? PGACCEL_UNSUPPORTED : PGACCEL_INVALID_ARGUMENT) && bytes == poison,
          "invalid control preserves output");
  }
  uint64_t bytes = poison;
  check(pgaccel_join_rows_workspace_bytes(nullptr, &bytes) == PGACCEL_INVALID_ARGUMENT && bytes == poison,
        "null request rejected");
  check(pgaccel_join_rows_workspace_bytes(&base, nullptr) == PGACCEL_INVALID_ARGUMENT, "null output rejected");
  check(pgaccel_join_rows_controlled_workspace_bytes(&base, nullptr, &bytes) == PGACCEL_INVALID_ARGUMENT && bytes == poison,
        "null control rejected");
  const auto* misaligned = static_cast<const uint8_t*>(storage.data) + 1;
  check(pgaccel_join_rows_workspace_bytes(reinterpret_cast<const pgaccel_join_rows_request*>(misaligned), &bytes) ==
            PGACCEL_INVALID_ARGUMENT && bytes == poison, "misaligned request rejected before read");
  check(pgaccel_join_rows_controlled_workspace_bytes(&base, reinterpret_cast<const pgaccel_join_control*>(misaligned), &bytes) ==
            PGACCEL_INVALID_ARGUMENT && bytes == poison, "misaligned control rejected before read");
  auto* misaligned_output = reinterpret_cast<uint64_t*>(static_cast<uint8_t*>(storage.data) + 1);
  check(pgaccel_join_rows_workspace_bytes(&base, misaligned_output) == PGACCEL_INVALID_ARGUMENT,
        "misaligned output rejected before write");
  check(pgaccel_join_rows_controlled_workspace_bytes(&base, &valid, misaligned_output) == PGACCEL_INVALID_ARGUMENT,
        "controlled misaligned output rejected before write");
  for (auto* destination : {static_cast<uint64_t*>(storage.data), static_cast<uint64_t*>(storage.data) + 1}) {
    check(pgaccel_join_rows_workspace_bytes(&base, destination) == PGACCEL_INVALID_ARGUMENT,
          "resident output alias rejected before write");
    check(pgaccel_join_rows_controlled_workspace_bytes(&base, &valid, destination) == PGACCEL_INVALID_ARGUMENT,
          "controlled resident output alias rejected before write");
  }
  auto request = base;
  check(pgaccel_join_rows_workspace_bytes(&request, &request.outer_rows) == PGACCEL_INVALID_ARGUMENT &&
            std::memcmp(&request, &base, sizeof(base)) == 0, "request output alias remains unchanged");
  check(pgaccel_join_rows_controlled_workspace_bytes(&request, &valid, &request.outer_rows) == PGACCEL_INVALID_ARGUMENT &&
            std::memcmp(&request, &base, sizeof(base)) == 0, "controlled request alias remains unchanged");
  auto control = valid;
  check(pgaccel_join_rows_controlled_workspace_bytes(&base, &control, &control.work_items) == PGACCEL_INVALID_ARGUMENT &&
            std::memcmp(&control, &valid, sizeof(valid)) == 0, "control output alias remains unchanged");
  check(pgaccel_join_rows_controlled_workspace_bytes(&base, &valid, &bytes) == PGACCEL_OK &&
            bytes == expected_bytes(base.outer_rows, base.inner_rows, true), "valid query recovers after rejections");
}
} // namespace

int main() {
  {
    InaccessibleRows storage;
    if (storage.data == MAP_FAILED) {
      std::perror("mmap protected join input");
      return 1;
    }
    valid_layouts(storage);
    invalid_layouts(storage);
  }
  std::printf("join layout: %u passed, %u failed (no GPU initialization)\n", passed, failed);
  return failed ? 1 : 0;
}

#include "pgaccel_olap.h"

#include <cstdint>
#include <cstdio>

namespace {

int failures = 0;

void check(bool condition, const char* label) {
  if (!condition) {
    std::fprintf(stderr, "FAIL: %s\n", label);
    ++failures;
  }
}

void check_space(int32_t space, const char* label) {
  void* ptr = nullptr;
  const pgaccel_status status = pgaccel_grouped_agg_workspace_alloc(4096, 256, space, &ptr);
  check(status == PGACCEL_OK, label);
  check(ptr != nullptr, "nonzero workspace allocation returned NULL");
  if (ptr != nullptr) {
    check(reinterpret_cast<uintptr_t>(ptr) % 256 == 0, "workspace alignment guarantee");
    pgaccel_grouped_agg_workspace_free(ptr);
  }
}

}  // namespace

int main() {
  check(pgaccel_init() == PGACCEL_OK, "GPU init");

  void* sentinel = reinterpret_cast<void*>(uintptr_t{1});
  check(pgaccel_grouped_agg_workspace_alloc(1, 3, PGACCEL_MEM_SPACE_SHARED_USM, &sentinel) ==
            PGACCEL_ERROR,
        "reject non-power-of-two alignment");
  check(sentinel == nullptr, "failed allocation canonicalizes out pointer");
  check(pgaccel_grouped_agg_workspace_alloc(1, 64, PGACCEL_MEM_SPACE_HOST, &sentinel) ==
            PGACCEL_ERROR,
        "reject HOST workspace");
  check(pgaccel_grouped_agg_workspace_alloc(1, 64, PGACCEL_MEM_SPACE_SHARED_USM, nullptr) ==
            PGACCEL_ERROR,
        "reject NULL out pointer");
  check(pgaccel_grouped_agg_workspace_alloc(0, 64, PGACCEL_MEM_SPACE_SHARED_USM, &sentinel) ==
            PGACCEL_OK,
        "zero-byte allocation succeeds");
  check(sentinel == nullptr, "zero-byte allocation returns NULL");

  check_space(PGACCEL_MEM_SPACE_SHARED_USM, "shared workspace allocation");
  check_space(PGACCEL_MEM_SPACE_DEVICE, "device workspace allocation");
  pgaccel_grouped_agg_workspace_free(nullptr);
  check(pgaccel_shutdown() == PGACCEL_OK, "GPU shutdown");

  if (failures != 0) {
    std::fprintf(stderr, "%d grouped workspace checks failed\n", failures);
    return 1;
  }
  std::printf("grouped workspace: all checks passed\n");
  return 0;
}

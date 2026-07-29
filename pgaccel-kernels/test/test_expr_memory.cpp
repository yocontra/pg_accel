#include <sys/wait.h>
#include <unistd.h>

#include <array>
#include <cerrno>
#include <cstdint>
#include <cstdio>
#include <cstdlib>
#include <cstring>

#include "pgaccel_expr.h"

namespace {

int checks = 0;
int failures = 0;

void check(bool condition, const char* label) {
  ++checks;
  if (!condition) {
    std::fprintf(stderr, "FAIL: %s\n", label);
    ++failures;
  }
}

void test_shared_lifecycle() {
  void* pointer = reinterpret_cast<void*>(uintptr_t{1});
  check(pgaccel_expr_shared_alloc(0, &pointer) == PGACCEL_OK,
        "zero-byte shared allocation succeeds");
  check(pointer == nullptr, "zero-byte shared allocation clears output");
  check(pgaccel_expr_shared_alloc(32, nullptr) == PGACCEL_ERROR,
        "shared allocation rejects null output");

  check(pgaccel_expr_shared_alloc(32, &pointer) == PGACCEL_OK, "shared allocation succeeds");
  check(pointer != nullptr, "shared allocation returns storage");
  if (pointer != nullptr) {
    const std::array<uint8_t, 32> input = {0,   1,   2,   3,  5,  8,   13,  21, 34, 55, 89,
                                           144, 233, 121, 98, 77, 61,  47,  35, 25, 17, 11,
                                           7,   4,   2,   1,  0,  255, 128, 64, 32, 16};
    std::array<uint8_t, 32> output = {};
    check(pgaccel_expr_device_copy_from_host(pointer, input.data(), input.size()) == PGACCEL_OK,
          "copy API writes shared allocation");
    check(pgaccel_expr_device_copy_to_host(output.data(), pointer, output.size()) == PGACCEL_OK,
          "copy API reads shared allocation");
    check(output == input, "shared allocation copy round trip is exact");
  }
  pgaccel_expr_shared_free(pointer);
  pgaccel_expr_shared_free(nullptr);
}

void test_device_lifecycle() {
  void* pointer = reinterpret_cast<void*>(uintptr_t{1});
  check(pgaccel_expr_device_alloc(0, &pointer) == PGACCEL_OK,
        "zero-byte device allocation succeeds");
  check(pointer == nullptr, "zero-byte device allocation clears output");
  check(pgaccel_expr_device_alloc(32, nullptr) == PGACCEL_ERROR,
        "device allocation rejects null output");

  check(pgaccel_expr_device_alloc(32, &pointer) == PGACCEL_OK, "device allocation succeeds");
  check(pointer != nullptr, "device allocation returns storage");
  if (pointer != nullptr) {
    const std::array<uint64_t, 4> first = {0, UINT64_MAX, 0x0123456789abcdefULL, 42};
    const std::array<uint64_t, 4> second = {91, 82, 73, 64};
    std::array<uint64_t, 4> observed = {};
    check(pgaccel_expr_device_copy_from_host(pointer, first.data(), sizeof(first)) == PGACCEL_OK,
          "host-to-device copy succeeds");
    check(pgaccel_expr_device_copy_to_host(observed.data(), pointer, sizeof(observed)) ==
              PGACCEL_OK,
          "device-to-host copy succeeds");
    check(observed == first, "device allocation round trip is exact");
    check(pgaccel_expr_device_copy_from_host(pointer, second.data(), sizeof(second)) == PGACCEL_OK,
          "host-to-device overwrite succeeds");
    check(pgaccel_expr_device_copy_to_host(observed.data(), pointer, sizeof(observed)) ==
              PGACCEL_OK,
          "device-to-host overwrite copy succeeds");
    check(observed == second, "device overwrite round trip is exact");
  }
  pgaccel_expr_device_free(pointer);
  pgaccel_expr_device_free(nullptr);
}

void test_alloc_copy_and_argument_contracts() {
  const std::array<int64_t, 5> input = {INT64_MIN, -1, 0, 1, INT64_MAX};
  std::array<int64_t, 5> output = {};
  void* pointer = reinterpret_cast<void*>(uintptr_t{1});

  check(pgaccel_expr_device_alloc_copy(nullptr, 0, &pointer) == PGACCEL_OK,
        "zero-byte allocation-copy succeeds");
  check(pointer == nullptr, "zero-byte allocation-copy clears output");
  check(pgaccel_expr_device_alloc_copy(input.data(), sizeof(input), nullptr) == PGACCEL_ERROR,
        "allocation-copy rejects null output");
  check(pgaccel_expr_device_alloc_copy(nullptr, sizeof(input), &pointer) == PGACCEL_ERROR,
        "allocation-copy rejects null source");
  check(pointer == nullptr, "failed allocation-copy clears output");

  check(pgaccel_expr_device_alloc_copy(input.data(), sizeof(input), &pointer) == PGACCEL_OK,
        "allocation-copy succeeds");
  check(pointer != nullptr, "allocation-copy returns storage");
  if (pointer != nullptr) {
    check(pgaccel_expr_device_copy_to_host(output.data(), pointer, sizeof(output)) == PGACCEL_OK,
          "allocation-copy result copies to host");
    check(output == input, "allocation-copy preserves exact bytes");
  }
  pgaccel_expr_device_free(pointer);

  check(pgaccel_expr_device_copy_from_host(nullptr, nullptr, 0) == PGACCEL_OK,
        "zero-byte host-to-device copy ignores pointers");
  check(pgaccel_expr_device_copy_to_host(nullptr, nullptr, 0) == PGACCEL_OK,
        "zero-byte device-to-host copy ignores pointers");
  check(pgaccel_expr_device_copy_from_host(nullptr, input.data(), 1) == PGACCEL_ERROR,
        "host-to-device copy rejects null destination");
  check(pgaccel_expr_device_copy_from_host(output.data(), nullptr, 1) == PGACCEL_ERROR,
        "host-to-device copy rejects null source");
  check(pgaccel_expr_device_copy_to_host(nullptr, input.data(), 1) == PGACCEL_ERROR,
        "device-to-host copy rejects null destination");
  check(pgaccel_expr_device_copy_to_host(output.data(), nullptr, 1) == PGACCEL_ERROR,
        "device-to-host copy rejects null source");
}

void test_no_device_paths() {
  const pgaccel_status init_status = pgaccel_init();
  check(init_status != PGACCEL_OK, "CPU-only visibility has no GPU device");
  if (init_status == PGACCEL_OK) {
    pgaccel_shutdown();
    return;
  }

  const uint8_t input = 42;
  uint8_t output = 0;
  void* pointer = reinterpret_cast<void*>(uintptr_t{1});
  check(pgaccel_expr_shared_alloc(1, &pointer) == init_status,
        "shared allocation propagates initialization failure");
  check(pointer == nullptr, "failed shared allocation clears output");
  pgaccel_expr_shared_free(reinterpret_cast<void*>(uintptr_t{1}));

  pointer = reinterpret_cast<void*>(uintptr_t{1});
  check(pgaccel_expr_device_alloc(1, &pointer) == init_status,
        "device allocation propagates initialization failure");
  check(pointer == nullptr, "failed device allocation clears output");

  pointer = reinterpret_cast<void*>(uintptr_t{1});
  check(pgaccel_expr_device_alloc_copy(&input, 1, &pointer) == init_status,
        "allocation-copy propagates initialization failure");
  check(pointer == nullptr, "failed allocation-copy clears output");
  check(pgaccel_expr_device_copy_from_host(&output, &input, 1) == init_status,
        "host-to-device copy propagates initialization failure");
  check(pgaccel_expr_device_copy_to_host(&output, &input, 1) == init_status,
        "device-to-host copy propagates initialization failure");
  pgaccel_expr_device_free(reinterpret_cast<void*>(uintptr_t{1}));

  check(pgaccel_shutdown() == PGACCEL_OK, "failed initialization shuts down cleanly");
}

bool run_no_device_child(const char* executable) {
  const pid_t child = fork();
  if (child < 0) {
    std::fprintf(stderr, "FAIL: fork no-device expr memory: errno=%d\n", errno);
    return false;
  }
  if (child == 0) {
    const char* visibility_mask = std::getenv("PGACCEL_TEST_NO_DEVICE_MASK");
    setenv("ACPP_VISIBILITY_MASK", visibility_mask != nullptr ? visibility_mask : "cuda", 1);
    setenv("PGACCEL_TEST_NO_DEVICE", "1", 1);
    execl(executable, executable, static_cast<char*>(nullptr));
    std::fprintf(stderr, "FAIL: exec no-device expr memory: errno=%d\n", errno);
    _exit(127);
  }

  int status = 0;
  pid_t waited;
  do {
    waited = waitpid(child, &status, 0);
  } while (waited < 0 && errno == EINTR);
  if (waited != child || !WIFEXITED(status) || WEXITSTATUS(status) != 0) {
    std::fprintf(stderr, "FAIL: no-device expr memory child: status=%d errno=%d\n", status, errno);
    return false;
  }
  return true;
}

}  // namespace

int main(int argc, char** argv) {
  if (std::getenv("PGACCEL_TEST_NO_DEVICE") != nullptr) {
    test_no_device_paths();
    std::printf("expr memory no-device: %d checks, %d failures\n", checks, failures);
    return failures == 0 ? 0 : 1;
  }

  check(argc > 0 && argv[0] != nullptr && run_no_device_child(argv[0]),
        "no-device expr memory child");
  check(pgaccel_init() == PGACCEL_OK, "runtime initializes");
  if (failures == 0) {
    test_shared_lifecycle();
    test_device_lifecycle();
    test_alloc_copy_and_argument_contracts();
  }
  check(pgaccel_shutdown() == PGACCEL_OK, "runtime shuts down");
  std::printf("expr memory: %d checks, %d failures\n", checks, failures);
  return failures == 0 ? 0 : 1;
}

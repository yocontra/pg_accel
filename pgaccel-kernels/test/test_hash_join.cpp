// Semantic coverage for the resident count-only hash join.

#include <sys/wait.h>
#include <unistd.h>

#include <cerrno>
#include <cstdint>
#include <cstdio>
#include <cstdlib>
#include <limits>
#include <vector>

#include "pgaccel_expr.h"
#include "pgaccel_ffi.h"
#include "pgaccel_hash_join.h"

namespace {

int passes = 0;
int failures = 0;

void check(bool condition, const char* label) {
  if (condition) {
    ++passes;
  } else {
    std::fprintf(stderr, "FAIL: %s\n", label);
    ++failures;
  }
}

void check_status(pgaccel_status actual, pgaccel_status expected, const char* label) {
  if (actual == expected) {
    ++passes;
  } else {
    std::fprintf(stderr, "FAIL: %s -- got %d, expected %d\n", label, static_cast<int>(actual),
                 static_cast<int>(expected));
    ++failures;
  }
}

class DeviceBuffer {
 public:
  DeviceBuffer() = default;
  DeviceBuffer(const DeviceBuffer&) = delete;
  DeviceBuffer& operator=(const DeviceBuffer&) = delete;

  ~DeviceBuffer() { pgaccel_expr_device_free(pointer_); }

  bool copy_from(const void* source, size_t bytes) {
    return pgaccel_expr_device_alloc_copy(source, bytes, &pointer_) == PGACCEL_OK &&
           pointer_ != nullptr;
  }

  void* get() const { return pointer_; }

 private:
  void* pointer_ = nullptr;
};

template <typename Key>
void check_count(const std::vector<Key>& build_keys, const std::vector<uint8_t>& build_nulls,
                 const std::vector<Key>& probe_keys, const std::vector<uint8_t>& probe_nulls,
                 pgaccel_key_type key_type, size_t expected, const char* label) {
  DeviceBuffer device_build_keys;
  DeviceBuffer device_build_nulls;
  DeviceBuffer device_probe_keys;
  DeviceBuffer device_probe_nulls;
  check(device_build_keys.copy_from(build_keys.data(), build_keys.size() * sizeof(Key)),
        "build key allocation");
  check(device_probe_keys.copy_from(probe_keys.data(), probe_keys.size() * sizeof(Key)),
        "probe key allocation");
  if (!build_nulls.empty()) {
    check(device_build_nulls.copy_from(build_nulls.data(), build_nulls.size()),
          "build null allocation");
  }
  if (!probe_nulls.empty()) {
    check(device_probe_nulls.copy_from(probe_nulls.data(), probe_nulls.size()),
          "probe null allocation");
  }
  if (device_build_keys.get() == nullptr || device_probe_keys.get() == nullptr ||
      (!build_nulls.empty() && device_build_nulls.get() == nullptr) ||
      (!probe_nulls.empty() && device_probe_nulls.get() == nullptr)) {
    return;
  }

  const uint64_t before = pgaccel_gpu_exec_count();
  pgaccel_hash_table* table = pgaccel_hash_join_build_device_count(
      device_build_keys.get(), static_cast<const uint8_t*>(device_build_nulls.get()),
      build_keys.size(), key_type);
  check(table != nullptr, label);
  if (table == nullptr)
    return;

  size_t count = std::numeric_limits<size_t>::max();
  const pgaccel_status status = pgaccel_hash_join_count_device(
      table, device_probe_keys.get(), static_cast<const uint8_t*>(device_probe_nulls.get()),
      probe_keys.size(), &count);
  check_status(status, PGACCEL_OK, "resident count status");
  check(count == expected, "resident count result");
  check(pgaccel_gpu_exec_count() >= before + 2, "resident build and count dispatch");
  pgaccel_hash_join_free(table);
}

uint64_t test_hash64(uint64_t key) {
  key ^= key >> 33;
  key *= 0xff51afd7ed558ccdULL;
  key ^= key >> 33;
  key *= 0xc4ceb9fe1a85ec53ULL;
  key ^= key >> 33;
  return key;
}

std::vector<int32_t> colliding_keys(size_t count, size_t capacity) {
  std::vector<int32_t> result;
  const uint64_t bucket = test_hash64(0) & (capacity - 1);
  for (uint32_t candidate = 0; result.size() < count; ++candidate) {
    if ((test_hash64(candidate) & (capacity - 1)) == bucket)
      result.push_back(static_cast<int32_t>(candidate));
  }
  return result;
}

void test_int32_duplicates_and_nulls() {
  const std::vector<int32_t> build = {1, 2, 2, 3, 3, 3, 99};
  const std::vector<uint8_t> build_nulls = {0, 0, 0, 0, 0, 0, 1};
  const std::vector<int32_t> probe = {2, 3, 4, 2, 3};
  const std::vector<uint8_t> probe_nulls = {0, 0, 0, 0, 1};
  check_count(build, build_nulls, probe, probe_nulls, PGACCEL_KEY_INT32, 7,
              "INT32 resident table build");
}

void test_int64_boundaries() {
  const int64_t min = std::numeric_limits<int64_t>::min();
  const int64_t max = std::numeric_limits<int64_t>::max();
  const std::vector<int64_t> build = {min, max, -1, 0, max, min};
  const std::vector<int64_t> probe = {max, min, 7, -1};
  check_count(build, {}, probe, {}, PGACCEL_KEY_INT64, 5, "INT64 resident table build");
}

void test_collision_chains() {
  constexpr size_t kDistinct = 12;
  constexpr size_t kRepetitions = 32;
  constexpr size_t kRows = kDistinct * kRepetitions;
  constexpr size_t kCapacity = 1024;
  const std::vector<int32_t> keys = colliding_keys(kDistinct, kCapacity);

  std::vector<int32_t> build;
  std::vector<uint8_t> nulls;
  build.reserve(kRows);
  nulls.reserve(kRows);
  std::vector<size_t> live_per_key(kDistinct, 0);
  for (size_t repetition = 0; repetition < kRepetitions; ++repetition) {
    for (size_t index = 0; index < kDistinct; ++index) {
      const bool is_null = ((repetition * kDistinct + index) % 19) == 0;
      build.push_back(keys[index]);
      nulls.push_back(static_cast<uint8_t>(is_null));
      if (!is_null)
        ++live_per_key[index];
    }
  }
  size_t expected = 0;
  for (size_t live : live_per_key)
    expected += live;
  check_count(build, nulls, keys, {}, PGACCEL_KEY_INT32, expected,
              "collision-heavy resident table build");

  size_t self_join_expected = 0;
  for (size_t live : live_per_key)
    self_join_expected += live * live;
  check_count(build, nulls, build, nulls, PGACCEL_KEY_INT32, self_join_expected,
              "collision-heavy resident self join build");
}

void test_empty_and_invalid_inputs() {
  int32_t key = 1;
  uint8_t null = 0;
  DeviceBuffer device_key;
  DeviceBuffer device_null;
  check(device_key.copy_from(&key, sizeof(key)), "invalid-input key allocation");
  check(device_null.copy_from(&null, sizeof(null)), "invalid-input null allocation");
  if (device_key.get() == nullptr || device_null.get() == nullptr)
    return;

  check(pgaccel_hash_join_build_device_count(nullptr, nullptr, 1, PGACCEL_KEY_INT32) == nullptr,
        "null resident build keys rejected");
  check(pgaccel_hash_join_build_device_count(device_key.get(), nullptr, 0, PGACCEL_KEY_INT32) ==
            nullptr,
        "empty resident build rejected");
  check(pgaccel_hash_join_build_device_count(
            device_key.get(), nullptr, static_cast<size_t>(std::numeric_limits<int32_t>::max()) + 1,
            PGACCEL_KEY_INT32) == nullptr,
        "oversized resident build rejected before dereference");
  check(pgaccel_hash_join_build_device_count(
            device_key.get(), nullptr, static_cast<size_t>(std::numeric_limits<int32_t>::max()),
            PGACCEL_KEY_INT32) == nullptr,
        "largest addressable build rejected when table capacity exceeds index range");
  check(pgaccel_hash_join_build_device_count(device_key.get(), nullptr, 1,
                                             static_cast<pgaccel_key_type>(2)) == nullptr,
        "unsupported resident key type rejected");

  pgaccel_hash_table* table = pgaccel_hash_join_build_device_count(
      device_key.get(), static_cast<const uint8_t*>(device_null.get()), 1, PGACCEL_KEY_INT32);
  check(table != nullptr, "validation table build");
  if (table == nullptr)
    return;

  size_t count = 77;
  check_status(pgaccel_hash_join_count_device(nullptr, device_key.get(), nullptr, 1, &count),
               PGACCEL_ERROR, "null table rejected");
  check_status(pgaccel_hash_join_count_device(table, nullptr, nullptr, 1, &count), PGACCEL_ERROR,
               "null probe keys rejected");
  check_status(pgaccel_hash_join_count_device(table, device_key.get(), nullptr, 1, nullptr),
               PGACCEL_ERROR, "null count output rejected");
  check_status(pgaccel_hash_join_count_device(table, device_key.get(), nullptr, 0, &count),
               PGACCEL_OK, "empty probe accepted");
  check(count == 0, "empty probe resets output");
  count = 77;
  check_status(pgaccel_hash_join_count_device(
                   table, device_key.get(), nullptr,
                   static_cast<size_t>(std::numeric_limits<uint32_t>::max()) + size_t{1}, &count),
               PGACCEL_UNSUPPORTED, "oversized probe rejected before dereference");
  check(count == 0, "oversized probe resets output");
  pgaccel_hash_join_free(table);
  pgaccel_hash_join_free(nullptr);
  ++passes;
}

void test_no_device_paths() {
  const pgaccel_status init_status = pgaccel_init();
  check(init_status != PGACCEL_OK, "no-device initialization rejected");
  if (init_status == PGACCEL_OK) {
    check_status(pgaccel_shutdown(), PGACCEL_OK, "unexpected no-device initialization shuts down");
    return;
  }

  const int32_t key = 1;
  check(pgaccel_hash_join_build_device_count(&key, nullptr, 1, PGACCEL_KEY_INT32) == nullptr,
        "resident build fails closed without a device");
  pgaccel_hash_join_free(nullptr);
  ++passes;
  check_status(pgaccel_shutdown(), PGACCEL_OK, "failed no-device initialization shuts down");
}

bool run_no_device_child(const char* executable) {
  const pid_t child = fork();
  if (child < 0) {
    std::fprintf(stderr, "FAIL: fork no-device hash join: errno=%d\n", errno);
    return false;
  }
  if (child == 0) {
    const char* visibility_mask = std::getenv("PGACCEL_TEST_NO_DEVICE_MASK");
    setenv("ACPP_VISIBILITY_MASK", visibility_mask != nullptr ? visibility_mask : "cuda", 1);
    setenv("PGACCEL_TEST_NO_DEVICE", "1", 1);
    execl(executable, executable, static_cast<char*>(nullptr));
    std::fprintf(stderr, "FAIL: exec no-device hash join: errno=%d\n", errno);
    _exit(127);
  }

  int status = 0;
  pid_t waited;
  do {
    waited = waitpid(child, &status, 0);
  } while (waited < 0 && errno == EINTR);
  if (waited != child || !WIFEXITED(status) || WEXITSTATUS(status) != 0) {
    std::fprintf(stderr, "FAIL: no-device hash join child: status=%d errno=%d\n", status, errno);
    return false;
  }
  return true;
}

}  // namespace

int main(int argc, char** argv) {
  std::printf("=== pgaccel resident hash join tests ===\n\n");

  if (std::getenv("PGACCEL_TEST_NO_DEVICE") != nullptr) {
    test_no_device_paths();
    std::printf("PASS=%d FAIL=%d\n", passes, failures);
    return failures == 0 ? 0 : 1;
  }

  check(argc > 0 && argv[0] != nullptr && run_no_device_child(argv[0]),
        "no-device hash join child");
  const pgaccel_status init = pgaccel_init();
  if (init != PGACCEL_OK) {
    std::fprintf(stderr, "FATAL: pgaccel_init failed with status %d\n", static_cast<int>(init));
    return 1;
  }

  test_int32_duplicates_and_nulls();
  test_int64_boundaries();
  test_collision_chains();
  test_empty_and_invalid_inputs();

  check_status(pgaccel_shutdown(), PGACCEL_OK, "runtime shuts down");
  std::printf("PASS=%d FAIL=%d\n", passes, failures);
  return failures == 0 ? 0 : 1;
}

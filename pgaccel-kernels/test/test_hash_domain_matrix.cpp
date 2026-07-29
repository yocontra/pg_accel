// Cross-domain semantic coverage for resident hash paths that are not exercised
// together by the narrower per-feature tests.

#include <algorithm>
#include <cstdint>
#include <cstdio>
#include <cstdlib>
#include <cstring>
#include <filesystem>
#include <fstream>
#include <limits>
#include <numeric>
#include <string>
#include <vector>

#include "pgaccel_expr.h"
#include "pgaccel_ffi.h"
#include "pgaccel_hash_join.h"
#include "pgaccel_resident_count.h"

namespace {

int failures = 0;

class EnvironmentRestore {
 public:
  EnvironmentRestore() {
    save("ACPP_APPDB_DIR");
    save("HOME");
  }

  EnvironmentRestore(const EnvironmentRestore&) = delete;
  EnvironmentRestore& operator=(const EnvironmentRestore&) = delete;

  ~EnvironmentRestore() {
    for (const Entry& entry : entries_) {
      if (entry.present)
        setenv(entry.name.c_str(), entry.value.c_str(), 1);
      else
        unsetenv(entry.name.c_str());
    }
  }

 private:
  struct Entry {
    std::string name;
    bool present;
    std::string value;
  };

  void save(const char* name) {
    const char* value = std::getenv(name);
    entries_.push_back({name, value != nullptr, value == nullptr ? "" : value});
  }

  std::vector<Entry> entries_;
};

void require(bool condition, const char* message) {
  if (!condition) {
    std::fprintf(stderr, "FAIL: %s\n", message);
    ++failures;
  }
}

template <typename T>
class DeviceBuffer {
 public:
  explicit DeviceBuffer(const std::vector<T>& values) : count_(values.size()) {
    const pgaccel_status status =
        pgaccel_expr_device_alloc_copy(values.data(), values.size() * sizeof(T), &pointer_);
    require(status == PGACCEL_OK, "device buffer allocation succeeds");
    require(pointer_ != nullptr, "device buffer allocation returns storage");
  }

  DeviceBuffer(const DeviceBuffer&) = delete;
  DeviceBuffer& operator=(const DeviceBuffer&) = delete;

  ~DeviceBuffer() { pgaccel_expr_device_free(pointer_); }

  T* get() { return static_cast<T*>(pointer_); }
  const T* get() const { return static_cast<const T*>(pointer_); }
  size_t size() const { return count_; }
  explicit operator bool() const { return pointer_ != nullptr; }

 private:
  void* pointer_ = nullptr;
  size_t count_ = 0;
};

template <typename Key>
void test_hash_join_width(pgaccel_key_type key_type, const char* label) {
  const std::vector<Key> build_keys = {
      static_cast<Key>(-17), static_cast<Key>(5),  static_cast<Key>(-17), static_cast<Key>(29),
      static_cast<Key>(5),   static_cast<Key>(91), static_cast<Key>(29),  static_cast<Key>(777),
  };
  const std::vector<uint8_t> build_nulls = {0, 0, 0, 0, 0, 1, 0, 0};
  const std::vector<Key> probe_keys = {
      static_cast<Key>(29),  static_cast<Key>(-17), static_cast<Key>(5),
      static_cast<Key>(404), static_cast<Key>(777), static_cast<Key>(29),
  };
  const std::vector<uint8_t> probe_nulls = {0, 0, 1, 0, 0, 0};

  DeviceBuffer<Key> device_build_keys(build_keys);
  DeviceBuffer<uint8_t> device_build_nulls(build_nulls);
  DeviceBuffer<Key> device_probe_keys(probe_keys);
  DeviceBuffer<uint8_t> device_probe_nulls(probe_nulls);
  if (!device_build_keys || !device_build_nulls || !device_probe_keys || !device_probe_nulls) {
    return;
  }

  pgaccel_reset_gpu_exec_count();
  pgaccel_hash_table* table = pgaccel_hash_join_build_device_count(
      device_build_keys.get(), device_build_nulls.get(), build_keys.size(), key_type);
  require(table != nullptr, "hash join resident count table builds");
  if (table == nullptr) {
    return;
  }
  size_t match_count = 0;
  const pgaccel_status status = pgaccel_hash_join_count_device(
      table, device_probe_keys.get(), device_probe_nulls.get(), probe_keys.size(), &match_count);
  require(status == PGACCEL_OK, "hash join resident count probe succeeds");
  require(match_count == 7, "hash join resident count preserves duplicate/null semantics");
  require(pgaccel_gpu_exec_count() >= 2, "hash join resident build and count dispatch on GPU");
  pgaccel_hash_join_free(table);
}

void test_h3_resident_count() {
  const std::vector<double> lat_exact = {37.7749, 40.7128, 37.7749, 47.6062, 40.7128, 37.7749};
  const std::vector<double> lng_exact = {-122.4194, -74.0060, -122.4194,
                                         -122.3321, -74.0060, -122.4194};
  std::vector<float> lat_f32(lat_exact.begin(), lat_exact.end());
  std::vector<float> lng_f32(lng_exact.begin(), lng_exact.end());
  DeviceBuffer<double> d_lat_exact(lat_exact);
  DeviceBuffer<double> d_lng_exact(lng_exact);
  DeviceBuffer<float> d_lat_f32(lat_f32);
  DeviceBuffer<float> d_lng_f32(lng_f32);
  if (!d_lat_exact || !d_lng_exact || !d_lat_f32 || !d_lng_f32) {
    return;
  }

  pgaccel_agg_state* state = nullptr;
  pgaccel_reset_gpu_exec_count();
  const pgaccel_status status =
      pgaccel_h3_lat_lng_count_resident_bulk(d_lat_exact.get(), d_lng_exact.get(), d_lat_f32.get(),
                                             d_lng_f32.get(), lat_exact.size(), 8, &state);
  require(status == PGACCEL_OK, "resident H3 COUNT succeeds");
  require(state != nullptr, "resident H3 COUNT returns aggregate state");
  require(pgaccel_gpu_exec_count() > 0, "resident H3 COUNT dispatches on GPU");
  if (state == nullptr) {
    return;
  }

  require(pgaccel_agg_group_count(state) == 3, "resident H3 COUNT identifies three cells");
  const double* counts = pgaccel_agg_get_results(state, 0);
  const int64_t* row_counts = pgaccel_agg_get_counts(state);
  require(counts != nullptr && row_counts != nullptr, "resident H3 COUNT exposes count buffers");
  if (counts != nullptr && row_counts != nullptr) {
    std::vector<int64_t> observed;
    int64_t total = 0;
    for (size_t group = 0; group < pgaccel_agg_group_count(state); ++group) {
      require(counts[group] == static_cast<double>(row_counts[group]),
              "resident H3 aggregate result matches row count");
      observed.push_back(row_counts[group]);
      total += row_counts[group];
    }
    std::sort(observed.begin(), observed.end());
    require(observed == std::vector<int64_t>({1, 2, 3}),
            "resident H3 COUNT preserves exact duplicate multiplicities");
    require(total == static_cast<int64_t>(lat_exact.size()),
            "resident H3 COUNT accounts for every valid row");
  }
  pgaccel_agg_free(state);
}

void test_hash_join_contract_boundaries() {
  size_t matches = 99;
  require(pgaccel_hash_join_build_device_count(nullptr, nullptr, 0, PGACCEL_KEY_INT64) == nullptr,
          "hash join rejects empty build");
  require(pgaccel_hash_join_build_device_count(reinterpret_cast<void*>(uintptr_t{1}), nullptr, 1,
                                               static_cast<pgaccel_key_type>(99)) == nullptr,
          "hash join rejects unknown key width before dereference");
  require(pgaccel_hash_join_count_device(nullptr, nullptr, nullptr, 0, &matches) == PGACCEL_ERROR,
          "hash join count rejects missing table");
  pgaccel_hash_join_free(nullptr);

  const std::vector<int64_t> build_keys = {7, 7, 11, 19};
  const std::vector<int64_t> probe_keys = {7};
  DeviceBuffer<int64_t> device_build(build_keys);
  DeviceBuffer<int64_t> device_probe(probe_keys);
  if (!device_build || !device_probe)
    return;

  pgaccel_hash_table* table = pgaccel_hash_join_build_device_count(
      device_build.get(), nullptr, build_keys.size(), PGACCEL_KEY_INT64);
  require(table != nullptr, "hash join boundary table builds");
  if (table == nullptr)
    return;

  matches = 99;
  require(pgaccel_hash_join_count_device(table, device_probe.get(), nullptr, 0, &matches) ==
              PGACCEL_OK,
          "hash join empty probe succeeds");
  require(matches == 0, "hash join empty probe publishes zero");
  require(pgaccel_hash_join_count_device(table, nullptr, nullptr, 1, &matches) == PGACCEL_ERROR,
          "hash join rejects missing probe keys");
  require(pgaccel_hash_join_count_device(table, device_probe.get(), nullptr, 1, nullptr) ==
              PGACCEL_ERROR,
          "hash join rejects missing match output");
  require(
      pgaccel_hash_join_count_device(table, device_probe.get(), nullptr,
                                     static_cast<size_t>(std::numeric_limits<uint32_t>::max()) + 1,
                                     &matches) == PGACCEL_UNSUPPORTED,
      "hash join rejects unaddressable probe count");
  pgaccel_hash_join_free(table);
}

void test_archive_observability() {
  char cache_dir[512] = {};
  const pgaccel_status dir_status = pgaccel_archive_jit_cache_dir(cache_dir, sizeof(cache_dir));
  require(dir_status == PGACCEL_OK, "archive cache directory resolves");
  require(cache_dir[0] != '\0', "archive cache directory is nonempty");
  if (dir_status == PGACCEL_OK) {
    std::error_code error;
    require(std::filesystem::is_directory(cache_dir, error) && !error,
            "archive cache directory exists after GPU dispatch");
  }

  pgaccel_archive_snapshot snapshot{};
  const pgaccel_status snapshot_status = pgaccel_archive_stats_snapshot(&snapshot);
  require(snapshot_status == PGACCEL_OK, "archive cache snapshot succeeds");
  require(snapshot.orphan_metallib <= snapshot.metallib_files,
          "archive orphan count cannot exceed metallib count");
  require(snapshot.metallib_files > 0, "archive snapshot observes compiled Metal libraries");
}

void test_host_runtime_contracts() {
  require(pgaccel_shutdown() == PGACCEL_OK, "shutdown is idempotent before initialization");

  require(pgaccel_archive_stats_snapshot(nullptr) == PGACCEL_ERROR,
          "archive snapshot rejects missing output");
  char buffer[1024] = {};
  require(pgaccel_archive_jit_cache_dir(nullptr, sizeof(buffer)) == PGACCEL_ERROR,
          "archive path rejects missing output");
  require(pgaccel_archive_jit_cache_dir(buffer, 0) == PGACCEL_ERROR,
          "archive path rejects zero capacity");

  EnvironmentRestore restore;
  std::error_code error;
  int uniqueness = 0;
  const std::filesystem::path root =
      std::filesystem::temp_directory_path(error) /
      ("pgaccel-archive-contract-" + std::to_string(reinterpret_cast<uintptr_t>(&uniqueness)));
  require(!error, "temporary archive root resolves");
  std::filesystem::remove_all(root, error);
  error.clear();

  const std::filesystem::path appdb = root / "apps";
  const std::filesystem::path cache = appdb / "global" / "jit-cache";
  require(setenv("ACPP_APPDB_DIR", appdb.c_str(), 1) == 0,
          "archive override environment is writable");
  unsetenv("HOME");

  require(pgaccel_archive_jit_cache_dir(buffer, sizeof(buffer)) == PGACCEL_OK,
          "archive path honors ACPP_APPDB_DIR");
  require(std::filesystem::path(buffer) == cache, "archive override path is exact");
  char tiny[2] = {'x', 'x'};
  require(pgaccel_archive_jit_cache_dir(tiny, sizeof(tiny)) == PGACCEL_ERROR && tiny[0] == '\0',
          "archive path rejects truncation and clears output");

  pgaccel_archive_snapshot snapshot{};
  require(pgaccel_archive_stats_snapshot(&snapshot) == PGACCEL_OK,
          "missing archive directory is an empty snapshot");
  require(snapshot.metallib_files == 0 && snapshot.metalar_files == 0 && snapshot.jit_files == 0 &&
              snapshot.orphan_metallib == 0,
          "missing archive directory reports zero files");

  std::filesystem::create_directories(cache, error);
  require(!error, "archive fixture directory is created");
  std::ofstream(cache / "paired.metallib").put('x');
  std::ofstream(cache / "paired.metalar").put('x');
  std::ofstream(cache / "orphan.metallib").put('x');
  std::ofstream(cache / "compile.jit").put('x');
  std::ofstream(cache / "ignored.txt").put('x');
  snapshot = {};
  require(pgaccel_archive_stats_snapshot(&snapshot) == PGACCEL_OK,
          "archive fixture snapshot succeeds");
  require(snapshot.metallib_files == 2 && snapshot.metalar_files == 1 && snapshot.jit_files == 1 &&
              snapshot.orphan_metallib == 1,
          "archive snapshot classifies paired, orphan, and JIT files");

  require(setenv("ACPP_APPDB_DIR", "", 1) == 0, "empty archive override is writable");
  unsetenv("HOME");
  snapshot.metallib_files = 99;
  require(pgaccel_archive_stats_snapshot(&snapshot) == PGACCEL_ERROR &&
              snapshot.metallib_files == 0,
          "archive snapshot rejects an unresolved cache root and clears output");
  buffer[0] = 'x';
  require(pgaccel_archive_jit_cache_dir(buffer, sizeof(buffer)) == PGACCEL_ERROR &&
              buffer[0] == '\0',
          "archive path rejects an unresolved cache root and clears output");

  std::filesystem::remove_all(root, error);
  require(!error, "archive fixture cleanup succeeds");
}

}  // namespace

int main() {
  std::printf("=== pgaccel hash domain matrix ===\n");
  test_host_runtime_contracts();
  if (pgaccel_init() != PGACCEL_OK) {
    std::fprintf(stderr, "FAIL: pgaccel_init\n");
    return 1;
  }

  test_hash_join_width<int32_t>(PGACCEL_KEY_INT32, "int32 hash join table builds");
  test_hash_join_width<int64_t>(PGACCEL_KEY_INT64, "int64 hash join table builds");
  test_hash_join_contract_boundaries();
  test_h3_resident_count();
  test_archive_observability();
  require(pgaccel_shutdown() == PGACCEL_OK, "runtime shutdown succeeds");

  std::printf("failures=%d\n", failures);
  return failures == 0 ? 0 : 1;
}

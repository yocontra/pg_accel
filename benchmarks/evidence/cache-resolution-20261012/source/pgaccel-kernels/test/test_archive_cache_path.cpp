// Host-only archive observation. Does not call pgaccel_init or dispatch kernels.
#include <unistd.h>

#include <cstdio>
#include <cstdlib>
#include <filesystem>
#include <fstream>
#include <string>

#include "pgaccel_ffi.h"
#include "../src/archive_cache_path.hpp"

int main(int argc, char** argv) {
  if (argc == 2 && std::string{argv[1]} == "--path") {
    char path[16384]{};
    if (pgaccel_archive_jit_cache_dir(path, sizeof(path)) != PGACCEL_OK)
      return 1;
    std::puts(path);
    return 0;
  }
  char temporary[] = "/tmp/pgaccel-archive-path-XXXXXX";
  const char* created = ::mkdtemp(temporary);
  if (!created)
    return 1;
  const std::filesystem::path root{created};
  const auto base = root / "override";
  ::setenv("ACPP_APPDB_DIR", base.c_str(), 1);
  ::setenv("HOME", (root / "home-decoy").c_str(), 1);
  ::unsetenv("XDG_DATA_HOME");
  char path[16384]{};
  int failures = 0;
  auto check = [&](bool good, const char* label) {
    if (!good) {
      std::fprintf(stderr, "FAIL: %s\n", label);
      ++failures;
    }
  };
  check(pgaccel_archive_jit_cache_dir(path, sizeof(path)) == PGACCEL_OK, "resolve override");
  const std::filesystem::path cache{path};
  check(cache.parent_path().parent_path() == base / "apps", "override contains apps layer");
#ifdef __APPLE__
  check(cache == base / "apps" / "global" / "jit-cache", "pinned Darwin global cache");
#endif
  check(!std::filesystem::exists(base), "resolution does not initialize persistent storage");
  check(pgaccel::archive_cache::path_for(base, "") == base / "apps/global/jit-cache",
        "empty executable selects global");
  const auto linux_a = pgaccel::archive_cache::path_for(base, "/first/postgres");
  const auto linux_b = pgaccel::archive_cache::path_for(base, "/second/postgres");
  check(linux_a != linux_b && linux_a.parent_path().filename().string().find("postgres-") == 0 &&
            linux_a.parent_path().parent_path() == base / "apps",
        "Linux algorithm hashes full path and retains executable basename");
  pgaccel_archive_snapshot snapshot{};
  check(pgaccel_archive_stats_snapshot(&snapshot) == PGACCEL_OK && snapshot.metallib_files == 0,
        "missing cache is empty without creating it");
  check(!std::filesystem::exists(base), "snapshot does not create missing directories");
  std::filesystem::create_directories(cache);
  std::ofstream(cache / "paired.metallib").put('x');
  std::ofstream(cache / "paired.metalar").put('x');
  std::ofstream(cache / "orphan.metallib").put('x');
  std::ofstream(cache / "compile.jit").put('x');
  const auto decoy = base / "global" / "jit-cache";
  std::filesystem::create_directories(decoy);
  std::ofstream(decoy / "decoy.metalar").put('x');
  check(pgaccel_archive_stats_snapshot(&snapshot) == PGACCEL_OK && snapshot.metallib_files == 2 &&
            snapshot.metalar_files == 1 && snapshot.jit_files == 1 && snapshot.orphan_metallib == 1,
        "counts actual cache, not old-layout decoy");
  char tiny[2] = {'x', 'x'};
  check(pgaccel_archive_jit_cache_dir(tiny, sizeof(tiny)) == PGACCEL_ERROR && tiny[0] == '\0',
        "truncation clears destination");
  check(pgaccel_archive_stats_snapshot(nullptr) == PGACCEL_ERROR, "null snapshot rejected");
  std::filesystem::permissions(cache, std::filesystem::perms::none);
  snapshot.metallib_files = 99;
  check(pgaccel_archive_stats_snapshot(&snapshot) == PGACCEL_ERROR && snapshot.metallib_files == 0 &&
            snapshot.metalar_files == 0 && snapshot.jit_files == 0 && snapshot.orphan_metallib == 0,
        "unreadable directory is an error with no partial counts");
  std::filesystem::permissions(cache, std::filesystem::perms::owner_all);
  std::filesystem::remove_all(cache);
  std::ofstream(cache).put('x');
  snapshot.metallib_files = 99;
  check(pgaccel_archive_stats_snapshot(&snapshot) == PGACCEL_ERROR && snapshot.metallib_files == 0,
        "non-directory cache is not an empty successful snapshot");
  std::filesystem::remove_all(root);
  std::printf("archive path host checks: %s\n", failures ? "FAIL" : "PASS");
  return failures ? 1 : 0;
}
